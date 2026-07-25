use super::*;
use crate::cli::{ModelsListSort, ModelsLoraBehaviorStrataPolicy};

#[test]
fn test_parses_models_list_query_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "list",
        "--provider",
        "anthropic",
        "--where",
        "tier=frontier,strengths=coding",
        "--where",
        "tool_support.parity=interchangeable",
        "--sort",
        "pricing.input",
        "--columns",
        "id,pricing.input,tool_support.parity_notes",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::List(list) = args.command else {
        panic!("expected models list command");
    };
    assert_eq!(list.provider.as_deref(), Some("anthropic"));
    assert_eq!(
        list.where_filters,
        vec![
            "tier=frontier,strengths=coding",
            "tool_support.parity=interchangeable"
        ]
    );
    assert!(matches!(list.sort, Some(ModelsListSort::PricingInput)));
    assert_eq!(
        list.columns.as_deref(),
        Some("id,pricing.input,tool_support.parity_notes")
    );
}

#[test]
fn test_parses_models_lora_preflight_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "preflight",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--tool-format",
        "text",
        "--corpus",
        "./lora-corpus",
        "--config",
        "./config/e4b.yaml",
        "--max-seq-length",
        "8192",
        "--min-fit-ratio",
        "0.98",
        "--hard-token-limit",
        "32768",
        "--min-records",
        "190",
        "--source-tool-format",
        "json",
        "--min-tool-call-share",
        "0.95",
        "--done-marker",
        "##DONE##",
        "--behavior-strata-policy",
        "legacy-unclassified",
        "--check",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Preflight(args) = args.command else {
        panic!("expected models lora preflight command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.tool_format, "text");
    assert_eq!(args.corpus, "./lora-corpus");
    assert_eq!(
        args.config
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./config/e4b.yaml")
    );
    assert_eq!(args.max_seq_length, Some(8192));
    assert_eq!(args.min_fit_ratio, Some(0.98));
    assert_eq!(args.hard_token_limit, 32_768);
    assert_eq!(args.min_records, 190);
    assert_eq!(args.source_tool_format, "json");
    assert_eq!(args.min_tool_call_share, 0.95);
    assert_eq!(args.done_marker.as_deref(), Some("##DONE##"));
    assert_eq!(
        args.behavior_strata_policy,
        ModelsLoraBehaviorStrataPolicy::LegacyUnclassified
    );
    assert!(args.check);
    assert!(args.json);
}
