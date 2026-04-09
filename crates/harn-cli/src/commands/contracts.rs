use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process;

use serde_json::json;

use crate::cli::{
    ContractsArgs, ContractsBundleArgs, ContractsCommand, ContractsHostCapabilitiesArgs,
    ContractsOutputArgs,
};
use crate::commands::check;
use crate::package::{self, CheckConfig};

fn print_json(value: &serde_json::Value, pretty: bool) {
    let output = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .unwrap_or_else(|error| {
        eprintln!("Failed to serialize JSON output: {error}");
        process::exit(1);
    });
    println!("{output}");
}

fn effective_config_for_targets(
    targets: &[PathBuf],
    host_capabilities: Option<&String>,
    bundle_root: Option<&String>,
) -> CheckConfig {
    let mut config = targets
        .first()
        .map(|path| package::load_check_config(Some(path)))
        .unwrap_or_default();
    if let Some(path) = host_capabilities {
        config.host_capabilities_path = Some(path.clone());
    }
    if let Some(path) = bundle_root {
        config.bundle_root = Some(path.clone());
    }
    config
}

fn builtin_contract_value(_args: &ContractsOutputArgs) -> serde_json::Value {
    let runtime: BTreeSet<String> = harn_vm::stdlib::stdlib_builtin_names()
        .into_iter()
        .collect();
    let parser = harn_parser::known_builtin_metadata()
        .map(|entry| {
            (
                entry.name.to_string(),
                entry
                    .return_types
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    let names = runtime
        .iter()
        .cloned()
        .chain(parser.keys().cloned())
        .collect::<BTreeSet<_>>();
    for name in names {
        let parser_known = parser.contains_key(&name);
        let runtime_registered = runtime.contains(&name);
        let alignment_status = match (parser_known, runtime_registered) {
            (true, true) => "matched",
            (true, false) => "parser_only",
            (false, true) => "runtime_only",
            (false, false) => unreachable!(),
        };
        rows.push(json!({
            "name": name,
            "parser_known": parser_known,
            "runtime_registered": runtime_registered,
            "return_types": parser.get(&name).cloned().unwrap_or_default(),
            "alignment_status": alignment_status,
        }));
    }
    json!({
        "version": 1,
        "builtins": rows,
    })
}

fn host_capabilities_value(args: &ContractsHostCapabilitiesArgs) -> serde_json::Value {
    let mut config = CheckConfig::default();
    if let Some(path) = args.host_capabilities.as_ref() {
        config.host_capabilities_path = Some(path.clone());
    }
    let capabilities = check::load_host_capabilities(&config);
    let sorted = capabilities
        .into_iter()
        .map(|(capability, ops)| {
            let mut ops = ops.into_iter().collect::<Vec<_>>();
            ops.sort();
            (capability, ops)
        })
        .collect::<BTreeMap<_, _>>();
    json!({
        "version": 1,
        "capabilities": sorted,
    })
}

fn bundle_contract_value(args: &ContractsBundleArgs) -> (serde_json::Value, bool) {
    let targets: Vec<&str> = args.targets.iter().map(String::as_str).collect();
    let files = check::collect_harn_targets(&targets);
    if files.is_empty() {
        eprintln!("No .harn files found");
        process::exit(1);
    }

    let config = effective_config_for_targets(
        &files,
        args.host_capabilities.as_ref(),
        args.bundle_root.as_ref(),
    );

    let mut failed = false;
    if args.verify {
        let cross_file_imports = check::collect_cross_file_imports(&files);
        for file in &files {
            let mut file_config = package::load_check_config(Some(file));
            if let Some(path) = args.host_capabilities.as_ref() {
                file_config.host_capabilities_path = Some(path.clone());
            }
            if let Some(path) = args.bundle_root.as_ref() {
                file_config.bundle_root = Some(path.clone());
            }
            let outcome = check::check_file_inner(file, &file_config, &cross_file_imports);
            failed |= outcome.should_fail(file_config.strict);
        }
    }

    let value = check::build_bundle_manifest(&files, &config);
    (value, failed)
}

pub(crate) async fn handle_contracts_command(args: ContractsArgs) {
    match args.command {
        ContractsCommand::Builtins(args) => {
            print_json(&builtin_contract_value(&args), args.pretty);
        }
        ContractsCommand::HostCapabilities(args) => {
            print_json(&host_capabilities_value(&args), args.pretty);
        }
        ContractsCommand::Bundle(args) => {
            let (value, failed) = bundle_contract_value(&args);
            print_json(&value, args.pretty);
            if failed {
                process::exit(1);
            }
        }
    }
}
