use super::*;
#[test]
fn provider_tool_probe_audit_accepts_an_explicit_false_json_flag() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-probe-audit",
        "--mode",
        "streaming",
        "--case",
        "parallel_tool_calls",
        "--request-profile",
        "parameter_edges",
        "--json=false",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolProbeAudit(args) = provider.command else {
        panic!("expected provider tool-probe-audit command");
    };
    assert!(!args.json);
}
