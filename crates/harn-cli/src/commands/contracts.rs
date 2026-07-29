use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process;

use serde_json::json;

use harn_builtin_meta::{
    BuiltinContract, BuiltinExposure, EffectAccess, EffectKind, ResourceSelector,
};

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
    let contracts = harn_vm::stdlib::all_builtin_manifest()
        .iter()
        .map(|entry| (entry.name.to_string(), entry.contract))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    let names = runtime
        .iter()
        .cloned()
        .chain(parser.keys().cloned())
        .chain(contracts.keys().cloned())
        .collect::<BTreeSet<_>>();
    for name in names {
        let parser_known = parser.contains_key(&name);
        let runtime_registered = runtime.contains(&name);
        let contract = contracts.get(&name).copied();
        let alignment_status = match (parser_known, runtime_registered, contract.is_some()) {
            (true, true, _) => "matched",
            (true, false, _) => "parser_only",
            (false, true, _) => "runtime_only",
            (false, false, true) => "contract_only",
            (false, false, false) => unreachable!(),
        };
        rows.push(json!({
            "name": name,
            "parser_known": parser_known,
            "runtime_registered": runtime_registered,
            "contract_declared": contract.is_some(),
            "return_types": parser.get(&name).cloned().unwrap_or_default(),
            "alignment_status": alignment_status,
            "contract": contract.map(contract_json),
        }));
    }
    json!({
        "version": 2,
        "builtins": rows,
    })
}

fn contract_json(contract: BuiltinContract) -> serde_json::Value {
    json!({
        "exposure": exposure_json(contract.exposure),
        "effects": contract.effects.iter().map(effect_json).collect::<Vec<_>>(),
    })
}

fn exposure_json(exposure: BuiltinExposure) -> serde_json::Value {
    match exposure {
        BuiltinExposure::Undeclared => json!({"kind": "undeclared"}),
        BuiltinExposure::PureGlobal => json!({"kind": "pure_global"}),
        BuiltinExposure::CapabilityFunction { authority_argument } => json!({
            "kind": "capability_function",
            "authority_argument": authority_argument,
        }),
        BuiltinExposure::HarnessMethod { capability, method } => json!({
            "kind": "harness_method",
            "capability": capability.field_name(),
            "handle_type": capability.type_name(),
            "method": method,
            "source_name": format!("harness.{}.{}", capability.field_name(), method),
        }),
        BuiltinExposure::PrivilegedWire => json!({"kind": "privileged_wire"}),
        BuiltinExposure::RuntimeInternal => json!({"kind": "runtime_internal"}),
    }
}

fn effect_json(effect: &harn_builtin_meta::EffectSpec) -> serde_json::Value {
    json!({
        "kind": effect_kind_name(effect.kind),
        "access": effect_access_name(effect.access),
        "resources": effect.resources.iter().map(resource_selector_json).collect::<Vec<_>>(),
    })
}

fn effect_kind_name(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Stdio => "stdio",
        EffectKind::Fs => "fs",
        EffectKind::Env => "env",
        EffectKind::Clock => "clock",
        EffectKind::Random => "random",
        EffectKind::Network => "network",
        EffectKind::Process => "process",
        EffectKind::Llm => "llm",
        EffectKind::Tool => "tool",
        EffectKind::Host => "host",
        EffectKind::Worker => "worker",
        EffectKind::Secret => "secret",
        EffectKind::Observability => "observability",
        EffectKind::Channel => "channel",
        EffectKind::State => "state",
    }
}

fn effect_access_name(access: EffectAccess) -> &'static str {
    match access {
        EffectAccess::Read => "read",
        EffectAccess::Write => "write",
        EffectAccess::Mutate => "mutate",
        EffectAccess::Observe => "observe",
    }
}

fn resource_selector_json(selector: &ResourceSelector) -> serde_json::Value {
    match selector {
        ResourceSelector::Argument(argument) => {
            json!({"kind": "argument", "argument": argument})
        }
        ResourceSelector::Field { argument, path } => json!({
            "kind": "field",
            "argument": argument,
            "path": path,
        }),
        ResourceSelector::EachArgument(argument) => {
            json!({"kind": "each_argument", "argument": argument})
        }
        ResourceSelector::Constant(value) => {
            json!({"kind": "constant", "value": value})
        }
        ResourceSelector::Dynamic => json!({"kind": "dynamic"}),
    }
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
        let module_graph = check::build_module_graph(&files);
        let cross_file_imports = check::collect_cross_file_imports(&module_graph);
        let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
        for file in &files {
            let mut file_config = package::load_check_config(Some(file));
            if let Some(path) = args.host_capabilities.as_ref() {
                file_config.host_capabilities_path = Some(path.clone());
            }
            if let Some(path) = args.bundle_root.as_ref() {
                file_config.bundle_root = Some(path.clone());
            }
            let outcome = check::check_file_inner(
                &mut analysis,
                file,
                &file_config,
                &cross_file_imports,
                &module_graph,
                false,
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_export_projects_typed_exposure_and_effect_contracts() {
        harn_vm::stdlib::force_link();
        let value = builtin_contract_value(&ContractsOutputArgs { pretty: false });
        assert_eq!(value["version"], 2);

        let builtins = value["builtins"].as_array().expect("builtin rows");
        let fs_read = builtins
            .iter()
            .find(|row| row["name"] == "__cap_fs_read_text")
            .expect("filesystem capability contract");
        assert_eq!(fs_read["alignment_status"], "contract_only");
        assert_eq!(fs_read["contract"]["exposure"]["kind"], "harness_method");
        assert_eq!(
            fs_read["contract"]["exposure"]["source_name"],
            "harness.fs.read_text"
        );
        assert_eq!(fs_read["contract"]["effects"][0]["kind"], "fs");
        assert_eq!(fs_read["contract"]["effects"][0]["access"], "read");
        assert_eq!(
            fs_read["contract"]["effects"][0]["resources"][0]["kind"],
            "argument"
        );

        let pure = builtins
            .iter()
            .find(|row| row["contract"]["exposure"]["kind"] == "pure_global")
            .expect("at least one pure builtin");
        assert_eq!(pure["contract"]["effects"], json!([]));
    }
}
