use super::*;

#[test]
fn parses_provider_tool_probe_format_override() {
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
    assert_eq!(args.provider, "ollama");
    assert_eq!(args.model, "devstral-small-2");
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:11434"));
    assert!(matches!(args.mode, ProviderToolProbeModeArg::NonStreaming));
    assert!(matches!(
        args.probe_case,
        ProviderToolProbeCaseArg::ParallelToolCalls
    ));
    assert!(matches!(
        args.tool_format,
        Some(ProviderToolProbeFormatArg::Json)
    ));
    assert_eq!(args.marker, "marker");
    assert_eq!(args.repeat, 5);
    assert_eq!(args.response_fixture, Some(PathBuf::from("fixture.json")));
    assert!(!args.dry_run_request);
    assert!(args.json);
}

#[test]
fn parses_provider_tool_calibrate_matrix() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-calibrate",
        "--route",
        "openai:gpt-5.4-mini",
        "--route",
        "ollama:qwen3:8b",
        "--tool-format",
        "text",
        "--case",
        "parallel_tool_calls",
        "--repeat",
        "4",
        "--output",
        "fitness.json",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolCalibrate(args) = provider.command else {
        panic!("expected provider tool-calibrate command");
    };
    assert_eq!(args.routes, vec!["openai:gpt-5.4-mini", "ollama:qwen3:8b"]);
    assert!(matches!(
        args.tool_formats.as_slice(),
        [ProviderToolProbeFormatArg::Text]
    ));
    assert!(matches!(
        args.probe_cases.as_slice(),
        [ProviderToolProbeCaseArg::ParallelToolCalls]
    ));
    assert_eq!(args.repeat, 4);
    assert_eq!(args.output, PathBuf::from("fitness.json"));
}
