use crate::cli::PluginPathArgs;
use std::path::{Path, PathBuf};

pub(crate) fn run(args: &PluginPathArgs, require_conformant: bool) {
    let root = Path::new(&args.path);
    let data_dir = args
        .data_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".plugin-data"));
    let report = harn_vm::agent_plugins::load_agent_plugin(root, data_dir);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("plugin report is serializable")
        );
    } else {
        print_human(&report);
    }
    if !report.accepted || (require_conformant && !report.conformant) {
        std::process::exit(1);
    }
}

fn print_human(report: &harn_vm::agent_plugins::AgentPluginLoadReport) {
    if let Some(plugin) = &report.plugin {
        println!("plugin: {}", plugin.manifest.name);
        println!("specification: {}", harn_vm::agent_plugins::SPEC_VERSION);
        println!("skills: {}", plugin.skills.len());
        println!("mcp_servers: {}", plugin.mcp_servers.len());
    }
    println!("accepted: {}", report.accepted);
    println!("conformant: {}", report.conformant);
    for item in &report.diagnostics {
        let component = item
            .component
            .as_deref()
            .map(|name| format!(" [{name}]"))
            .unwrap_or_default();
        eprintln!(
            "{:?} {}{}: {}",
            item.severity, item.code, component, item.message
        );
    }
}
