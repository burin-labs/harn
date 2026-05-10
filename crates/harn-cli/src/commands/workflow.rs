use std::fs;
use std::path::Path;

use harn_vm::orchestration::{
    enforce_nested_invocation_ceiling, load_workflow_bundle, safe_function_tools,
    validate_workflow_patch, NestedInvocationTarget, SafeFunctionToolDescriptor, WorkflowPatch,
};

use crate::cli::{
    WorkflowArgs, WorkflowCommand, WorkflowFunctionToolsArgs, WorkflowNestedCeilingArgs,
    WorkflowPatchApplyArgs, WorkflowPatchCommand, WorkflowPatchPreviewArgs,
    WorkflowPatchValidateArgs,
};

pub(crate) fn handle(args: WorkflowArgs) -> Result<i32, String> {
    match args.command {
        WorkflowCommand::Patch(command) => handle_patch(command),
        WorkflowCommand::FunctionTools(args) => handle_function_tools(args),
        WorkflowCommand::NestedCeiling(args) => handle_nested_ceiling(args),
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

fn handle_patch(command: WorkflowPatchCommand) -> Result<i32, String> {
    match command {
        WorkflowPatchCommand::Validate(args) => handle_patch_validate(args),
        WorkflowPatchCommand::Apply(args) => handle_patch_apply(args),
        WorkflowPatchCommand::Preview(args) => handle_patch_preview(args),
    }
}

fn handle_patch_validate(args: WorkflowPatchValidateArgs) -> Result<i32, String> {
    let bundle = load_workflow_bundle(Path::new(&args.bundle))
        .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
    let patch = load_patch(&args.patch)?;
    let parent_ceiling = match args.parent_ceiling.as_deref() {
        Some(path) => Some(load_capability_policy(path)?),
        None => None,
    };
    let report = validate_workflow_patch(&bundle, &patch, parent_ceiling.as_ref());
    if args.json {
        print_json(&report)?;
    } else {
        print_patch_report(&report);
    }
    Ok(if report.valid { 0 } else { 1 })
}

fn handle_patch_apply(args: WorkflowPatchApplyArgs) -> Result<i32, String> {
    let bundle = load_workflow_bundle(Path::new(&args.bundle))
        .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
    let patch = load_patch(&args.patch)?;
    let parent_ceiling = match args.parent_ceiling.as_deref() {
        Some(path) => Some(load_capability_policy(path)?),
        None => None,
    };
    let report = validate_workflow_patch(&bundle, &patch, parent_ceiling.as_ref());
    if !report.valid {
        if args.json {
            print_json(&report)?;
        } else {
            print_patch_report(&report);
        }
        return Ok(1);
    }
    let patched =
        harn_vm::orchestration::apply_workflow_patch(&bundle, &patch).map_err(|errors| {
            errors
                .iter()
                .map(|err| format!("{}: {}", err.path, err.message))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let serialized = serde_json::to_string_pretty(&patched)
        .map_err(|error| format!("failed to serialize patched bundle: {error}"))?;
    fs::write(&args.out, &serialized)
        .map_err(|error| format!("failed to write patched bundle {}: {error}", args.out))?;
    if args.json {
        print_json(&report)?;
    } else {
        println!(
            "wrote patched workflow bundle {} → {} ({})",
            patched.id, args.out, report.bundle_validation.graph_digest
        );
        print_patch_report(&report);
    }
    Ok(0)
}

fn handle_patch_preview(args: WorkflowPatchPreviewArgs) -> Result<i32, String> {
    let bundle = load_workflow_bundle(Path::new(&args.bundle))
        .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
    let patch = load_patch(&args.patch)?;
    let report = validate_workflow_patch(&bundle, &patch, None);
    if args.json {
        print_json(&report)?;
    } else if args.mermaid {
        println!("{}", report.graph_export.mermaid);
    } else {
        print_patch_report(&report);
    }
    Ok(if report.bundle_validation.valid { 0 } else { 1 })
}

fn handle_function_tools(args: WorkflowFunctionToolsArgs) -> Result<i32, String> {
    let descriptors: Vec<SafeFunctionToolDescriptor> = safe_function_tools()
        .into_iter()
        .map(|tool| tool.descriptor)
        .collect();
    if args.json {
        print_json(&descriptors)?;
    } else {
        for descriptor in &descriptors {
            println!(
                "{} ({:?}, {:?})",
                descriptor.name,
                descriptor.annotations.kind,
                descriptor.annotations.side_effect_level,
            );
            println!("  {}", descriptor.description);
        }
    }
    Ok(0)
}

fn handle_nested_ceiling(args: WorkflowNestedCeilingArgs) -> Result<i32, String> {
    let bundle = load_workflow_bundle(Path::new(&args.bundle))
        .map_err(|error| format!("failed to load workflow bundle: {error}"))?;
    let parent = load_capability_policy(&args.parent)?;
    let report = enforce_nested_invocation_ceiling(
        &parent,
        &NestedInvocationTarget::WorkflowBundle(&bundle),
    );
    if args.json {
        print_json(&report)?;
    } else if report.allowed() {
        println!(
            "nested invocation allowed: bundle {} fits inside parent ceiling",
            report.target_label
        );
    } else {
        println!(
            "nested invocation rejected: bundle {} widens parent ceiling",
            report.target_label
        );
        for violation in &report.violations {
            println!("  [{}] {}", violation.kind, violation.detail);
        }
    }
    Ok(if report.allowed() { 0 } else { 1 })
}

fn load_patch(path: &str) -> Result<WorkflowPatch, String> {
    let bytes = fs::read(path).map_err(|error| format!("failed to read patch {path}: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("failed to parse patch {path}: {error}"))
}

fn load_capability_policy(path: &str) -> Result<harn_vm::orchestration::CapabilityPolicy, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read parent ceiling {path}: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse parent ceiling {path}: {error}"))
}

fn print_patch_report(report: &harn_vm::orchestration::WorkflowPatchValidationReport) {
    if report.valid {
        println!(
            "patch {} applies cleanly to bundle {} ({})",
            report.patch_id, report.bundle_id, report.bundle_validation.graph_digest
        );
    } else {
        println!(
            "patch {} cannot apply to bundle {}",
            report.patch_id, report.bundle_id
        );
    }
    for diagnostic in &report.apply_errors {
        let op_label = match (&diagnostic.op, diagnostic.op_index) {
            (Some(op), Some(index)) => format!("[{op} #{index}] "),
            (Some(op), None) => format!("[{op}] "),
            _ => String::new(),
        };
        println!(
            "  apply error {op_label}{}: {}",
            diagnostic.path, diagnostic.message
        );
    }
    for diagnostic in &report.bundle_validation.errors {
        println!("  bundle error {}: {}", diagnostic.path, diagnostic.message);
    }
    for diagnostic in &report.bundle_validation.warnings {
        println!(
            "  bundle warning {}: {}",
            diagnostic.path, diagnostic.message
        );
    }
    if !report.graph_diff.added_nodes.is_empty() {
        println!(
            "  added nodes: {}",
            report.graph_diff.added_nodes.join(", ")
        );
    }
    if !report.graph_diff.added_edges.is_empty() {
        println!("  added edges:");
        for edge in &report.graph_diff.added_edges {
            let branch = edge
                .branch
                .as_deref()
                .map(|b| format!(" [{b}]"))
                .unwrap_or_default();
            println!("    {} -> {}{branch}", edge.from, edge.to);
        }
    }
    if !report.graph_diff.updated_nodes.is_empty() {
        println!(
            "  updated nodes: {}",
            report.graph_diff.updated_nodes.join(", ")
        );
    }
    if !report.graph_diff.updated_capsules.is_empty() {
        println!(
            "  updated prompt capsules: {}",
            report.graph_diff.updated_capsules.join(", ")
        );
    }
    if !report.graph_diff.policy_fields_changed.is_empty() {
        println!(
            "  policy fields changed: {}",
            report.graph_diff.policy_fields_changed.join(", ")
        );
    }
    for violation in &report.capability_delta.widening {
        println!(
            "  ceiling widening [{}]: {}",
            violation.kind, violation.detail
        );
    }
}
