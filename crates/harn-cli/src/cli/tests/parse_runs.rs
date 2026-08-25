use super::*;

#[test]
fn test_parses_runs_review_provenance_inputs() {
    let cli = Cli::parse_from([
        "harn",
        "runs",
        "review",
        "--report",
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
    assert_eq!(review.report, Some(PathBuf::from("run-report.json")));
    assert_eq!(review.run_record, None);
    assert_eq!(review.events_db, None);
    assert_eq!(review.rubric, Some(PathBuf::from("rubric.md")));
    assert_eq!(review.model.as_deref(), Some("gpt-5.6-luna"));
}

#[test]
fn test_parses_runs_review_from_run_record() {
    let cli = Cli::parse_from([
        "harn",
        "runs",
        "review",
        "--run-record",
        "root.json",
        "--events-db",
        "events.sqlite",
    ]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::Review(review) = args.command else {
        panic!("expected runs review command");
    };
    assert_eq!(review.report, None);
    assert_eq!(review.run_record, Some(PathBuf::from("root.json")));
    assert_eq!(review.events_db, Some(PathBuf::from("events.sqlite")));
}

#[test]
fn test_runs_review_requires_exactly_one_typed_input() {
    assert!(Cli::try_parse_from(["harn", "runs", "review"]).is_err());
    assert!(Cli::try_parse_from([
        "harn",
        "runs",
        "review",
        "--report",
        "report.json",
        "--run-record",
        "run.json",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "harn",
        "runs",
        "review",
        "--report",
        "report.json",
        "--events-db",
        "events.sqlite",
    ])
    .is_err());
}

/// Every `harn runs` surface has to accept a session, or the gap this closes
/// stays open on whichever one was missed. Asserting them together is what
/// makes adding a sixth subcommand without `--from-session` a test failure
/// rather than a silent omission.
#[test]
fn every_runs_surface_accepts_a_session_instead_of_a_path() {
    for (subcommand, extract) in [
        (
            "inspect",
            (|args: RunsCommand| match args {
                RunsCommand::Inspect(inspect) => (inspect.path, inspect.source),
                other => panic!("expected inspect, got {other:?}"),
            })
                as fn(RunsCommand) -> (Option<String>, crate::cli::run_source::SessionSourceArgs),
        ),
        ("view", |args| match args {
            RunsCommand::View(view) => (view.path, view.source),
            other => panic!("expected view, got {other:?}"),
        }),
        ("report", |args| match args {
            RunsCommand::Report(report) => (report.path, report.source),
            other => panic!("expected report, got {other:?}"),
        }),
        ("export-training", |args| match args {
            RunsCommand::ExportTraining(export) => (export.path, export.source),
            other => panic!("expected export-training, got {other:?}"),
        }),
    ] {
        let cli = Cli::parse_from([
            "harn",
            "runs",
            subcommand,
            "--from-session",
            "019fc7e6-3103-7610-81ed-91599858fa1a",
        ]);
        let Command::Runs(args) = cli.command.unwrap() else {
            panic!("expected runs command for {subcommand}");
        };
        let (path, source) = extract(args.command);
        assert_eq!(
            path, None,
            "{subcommand} must not require a positional path alongside --from-session"
        );
        assert_eq!(
            source.from_session.as_deref(),
            Some("019fc7e6-3103-7610-81ed-91599858fa1a"),
            "{subcommand} must carry the session through"
        );
    }
}

#[test]
fn runs_review_accepts_a_session_as_a_third_exclusive_input() {
    let cli = Cli::parse_from(["harn", "runs", "review", "--from-session", "session-1"]);
    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::Review(review) = args.command else {
        panic!("expected runs review command");
    };
    assert_eq!(review.source.from_session.as_deref(), Some("session-1"));

    // Exclusive with both existing inputs, which is the property the shared
    // arg group buys over per-pair `conflicts_with` attributes.
    for conflicting in [
        vec!["--report", "report.json"],
        vec!["--run-record", "run.json"],
    ] {
        let mut argv = vec!["harn", "runs", "review", "--from-session", "session-1"];
        argv.extend(conflicting);
        assert!(
            Cli::try_parse_from(&argv).is_err(),
            "--from-session must conflict with {argv:?}"
        );
    }
}

#[test]
fn a_runs_surface_still_requires_naming_some_run() {
    // Dropping the positional path must not have made it optional outright:
    // with neither a path nor a session there is no run to report on.
    for subcommand in ["inspect", "view", "report", "export-training"] {
        assert!(
            Cli::try_parse_from(["harn", "runs", subcommand]).is_err(),
            "{subcommand} must reject naming no run at all"
        );
    }
}

#[test]
fn session_root_is_only_meaningful_alongside_a_session() {
    assert!(Cli::try_parse_from([
        "harn",
        "runs",
        "report",
        "root.json",
        "--session-root",
        "/tmp/workspace",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "harn",
        "runs",
        "report",
        "--from-session",
        "session-1",
        "--session-root",
        "/tmp/workspace",
    ])
    .is_ok());
}
