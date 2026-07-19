use super::*;
use crate::cli::ModelsListSort;

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
