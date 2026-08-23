use super::*;

#[test]
fn run_help_does_not_advertise_reserved_live_backend() {
    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("merge-captain")
        .and_then(|merge_captain| merge_captain.find_subcommand_mut("run"))
        .expect("merge-captain run subcommand exists")
        .render_long_help()
        .to_string();

    assert!(help.contains("mock"), "help must advertise mock: {help}");
    assert!(
        help.contains("replay"),
        "help must advertise replay: {help}"
    );
    assert!(
        !help.contains("live"),
        "help must not advertise the unavailable live backend: {help}"
    );
}

#[test]
fn live_backend_remains_an_explicit_reserved_value() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "run",
        "--backend",
        "live",
        "--once",
    ]);
    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let MergeCaptainCommand::Run(run) = args.command else {
        panic!("expected merge-captain run command");
    };
    assert!(matches!(
        run.backend,
        crate::cli::MergeCaptainBackendKind::Live
    ));
}
