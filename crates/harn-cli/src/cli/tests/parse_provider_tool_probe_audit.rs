use super::*;

#[test]
fn test_parses_provider_tool_probe_audit_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-probe-audit",
        "--mode",
        "streaming",
        "--case",
        "large_string_argument",
        "--json=false",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolProbeAudit(args) = provider.command else {
        panic!("expected provider tool-probe-audit command");
    };
    assert!(matches!(args.mode, ProviderToolProbeModeArg::Streaming));
    assert_eq!(args.probe_cases.len(), 1);
    assert!(matches!(
        args.probe_cases[0],
        ProviderToolProbeCaseArg::LargeStringArgument
    ));
    assert!(!args.json);
}
