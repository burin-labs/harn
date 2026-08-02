use super::*;

#[test]
fn test_parses_runs_inspect_compare() {
    let cli = Cli::parse_from([
        "harn",
        "runs",
        "inspect",
        "run.json",
        "--compare",
        "baseline.json",
    ]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::Inspect(inspect) = args.command else {
        panic!("expected runs inspect command");
    };
    assert_eq!(inspect.path, "run.json");
    assert_eq!(inspect.compare.as_deref(), Some("baseline.json"));
}

#[test]
fn test_parses_runs_view_json_session() {
    let cli = Cli::parse_from(["harn", "runs", "view", "runs", "--session", "--json"]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::View(view) = args.command else {
        panic!("expected runs view command");
    };
    assert_eq!(view.path, "runs");
    assert!(view.session);
    assert!(view.json);
}

#[test]
fn test_parses_runs_report_sources() {
    let cli = Cli::parse_from([
        "harn",
        "runs",
        "report",
        "root.json",
        "--events-db",
        "events.sqlite",
    ]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::Report(report) = args.command else {
        panic!("expected runs report command");
    };
    assert_eq!(report.path, "root.json");
    assert_eq!(
        report.events_db.as_deref(),
        Some(std::path::Path::new("events.sqlite"))
    );
}

#[test]
fn test_parses_runs_review_provenance_inputs() {
    let cli = Cli::parse_from([
        "harn",
        "runs",
        "review",
        "run-report.json",
        "--rubric",
        "rubric.md",
        "--model",
        "gpt-5.6-luna",
    ]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::Review(review) = args.command else {
        panic!("expected runs review command");
    };
    assert_eq!(review.path, PathBuf::from("run-report.json"));
    assert_eq!(review.rubric, Some(PathBuf::from("rubric.md")));
    assert_eq!(review.model.as_deref(), Some("gpt-5.6-luna"));
}
