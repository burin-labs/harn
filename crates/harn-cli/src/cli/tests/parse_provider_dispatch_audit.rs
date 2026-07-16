use super::*;

#[test]
fn test_parses_provider_dispatch_audit_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "dispatch-audit",
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet-4-6",
        "--route",
        "anthropic:claude-sonnet-4-6",
        "--capability",
        "tools",
        "--variant",
        "thinking",
        "--variant",
        "native",
        "--include-tool-probe-plan",
        "--tool-probe-case",
        "tool_result_followup",
        "--tool-probe-mode",
        "non-streaming",
        "--tool-probe-repeat",
        "3",
        "--tool-probe-timeout-secs",
        "45",
        "--tool-probe-output-dir",
        ".harn-runs/provider-live-probes/smoke",
        "--json",
    ]);

    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::DispatchAudit(args) = provider.command else {
        panic!("expected provider dispatch-audit command");
    };
    assert_eq!(args.providers, vec!["anthropic"]);
    assert_eq!(args.models, vec!["claude-sonnet-4-6"]);
    assert_eq!(args.routes, vec!["anthropic:claude-sonnet-4-6"]);
    assert_eq!(args.capabilities, vec!["tools"]);
    assert_eq!(
        args.variants,
        vec![
            ProviderDispatchAuditVariantArg::Thinking,
            ProviderDispatchAuditVariantArg::Native
        ]
    );
    assert!(args.include_tool_probe_plan);
    assert_eq!(
        args.tool_probe_cases,
        vec![ProviderToolProbeCaseArg::ToolResultFollowup]
    );
    assert_eq!(args.tool_probe_mode, ProviderToolProbeModeArg::NonStreaming);
    assert_eq!(args.tool_probe_repeat, 3);
    assert_eq!(args.tool_probe_timeout_secs, 45);
    assert_eq!(
        args.tool_probe_output_dir.as_deref(),
        Some(".harn-runs/provider-live-probes/smoke")
    );
    assert!(args.json);
}
