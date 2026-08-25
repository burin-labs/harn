use super::*;

#[test]
fn provider_tool_probe_keeps_output_and_execution_defaults() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-probe",
        "ollama",
        "--model",
        "devstral-small-2",
        "--base-url",
        "http://127.0.0.1:11434",
        "--mode",
        "non-streaming",
        "--case",
        "parallel_tool_calls",
        "--tool-format",
        "json",
        "--marker",
        "marker",
        "--repeat",
        "5",
        "--response-fixture",
        "fixture.json",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolProbe(args) = provider.command else {
        panic!("expected provider tool-probe command");
    };
    assert!(!args.dry_run_request);
    assert!(args.json);
}
