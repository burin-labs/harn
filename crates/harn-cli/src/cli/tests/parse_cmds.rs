use super::*;

#[test]
fn test_run_rejects_deny_allow_conflict() {
    let error = Cli::try_parse_from([
        "harn",
        "run",
        "--deny",
        "read_file",
        "--allow",
        "exec",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_run_project_handler_initialization_mode_is_a_clean_cutover() {
    let cli = Cli::parse_from(["harn", "run", "main.harn"]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(!args.eager_project_handlers);

    let cli = Cli::parse_from(["harn", "run", "--eager-project-handlers", "main.harn"]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.eager_project_handlers);
    assert_eq!(
        Cli::try_parse_from(["harn", "run", "--defer-project-handlers", "main.harn"])
            .unwrap_err()
            .kind(),
        clap::error::ErrorKind::UnknownArgument
    );
}

#[test]
fn test_environment_policy_is_independent_of_no_sandbox() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--no-sandbox",
        "--grant",
        "t=env:X",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.sandbox.no_sandbox);
    assert_eq!(args.sandbox.grant, ["t=env:X"]);

    let cli = Cli::parse_from([
        "harn",
        "run",
        "--no-sandbox",
        "--environment-policy",
        "isolated",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(
        args.sandbox.environment_policy,
        Some(crate::commands::run::EnvironmentPolicyArg::Isolated)
    );
}

#[test]
fn test_run_parses_sandbox_roots_and_rejects_no_sandbox_conflict() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--write-root",
        "../receipts",
        "--writable-root",
        "/tmp/cache",
        "--read-only-root",
        "../shared",
        "--sandbox-read-root",
        "/opt/sdk",
        "--sandbox-write-root",
        "/tmp/tool-cache",
        "--allow-process-network",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(
        args.sandbox.write_root,
        [PathBuf::from("../receipts"), PathBuf::from("/tmp/cache")]
    );
    assert_eq!(args.sandbox.read_only_root, [PathBuf::from("../shared")]);
    assert_eq!(args.sandbox.sandbox_read_root, [PathBuf::from("/opt/sdk")]);
    assert_eq!(
        args.sandbox.sandbox_write_root,
        [PathBuf::from("/tmp/tool-cache")]
    );

    for flag in ["--write-root", "--sandbox-write-root", "--read-only-root"] {
        let error =
            Cli::try_parse_from(["harn", "run", "--no-sandbox", flag, "../root", "main.harn"])
                .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
    assert_eq!(
        Cli::try_parse_from([
            "harn",
            "run",
            "--no-sandbox",
            "--allow-process-network",
            "main.harn",
        ])
        .unwrap_err()
        .kind(),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn test_time_run_shares_the_run_confinement_surface() {
    let flags = [
        "--environment-policy",
        "granted",
        "--grant",
        "token=env:TOKEN",
        "--write-root",
        "../receipts",
        "--allow-process-network",
    ];
    let run = {
        let mut argv = vec!["harn", "run"];
        argv.extend(flags);
        argv.push("main.harn");
        let Command::Run(args) = Cli::parse_from(argv).command.unwrap() else {
            panic!("expected run command");
        };
        args.sandbox
    };
    let timed = {
        let mut argv = vec!["harn", "time", "run"];
        argv.extend(flags);
        argv.push("main.harn");
        let Command::Time(args) = Cli::parse_from(argv).command.unwrap() else {
            panic!("expected time command");
        };
        let TimeCommand::Run(args) = args.command;
        args.sandbox
    };
    assert_eq!(timed.environment_policy, run.environment_policy);
    assert_eq!(timed.grant, run.grant);
    assert_eq!(timed.write_root, run.write_root);
    assert_eq!(timed.allow_process_network, run.allow_process_network);
}

#[test]
fn run_and_playground_keep_mock_inputs_exclusive() {
    for argv in [
        ["harn", "run", "--llm-mock", "fixtures.jsonl", "main.harn"].as_slice(),
        ["harn", "playground", "--llm-mock", "fixtures.jsonl"].as_slice(),
    ] {
        let cli = Cli::parse_from(argv);
        match cli.command.unwrap() {
            Command::Run(args) => assert!(args.llm_mock_record.is_none()),
            Command::Playground(args) => assert!(args.llm_mock_record.is_none()),
            _ => panic!("expected mock-capable command"),
        }
    }

    let cli = Cli::parse_from([
        "harn",
        "run",
        "--llm-mock-record",
        "recording.jsonl",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.llm_mock.is_none());
}

#[test]
fn run_output_destinations_stay_independent() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--emit-summary-json",
        "--summary-file",
        "summary.jsonl",
        "--emit-phase-json",
        "--phase-file",
        "phases.jsonl",
        "--emit-rusage-json",
        "--rusage-fd",
        "4",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.summary_fd.is_none());
    assert!(args.phase_fd.is_none());
    assert!(args.rusage_file.is_none());
}

#[test]
fn test_test_bench_run_help_discloses_wasi_feature_gate() {
    let help = Cli::command()
        .find_subcommand_mut("test-bench")
        .and_then(|test_bench| test_bench.find_subcommand_mut("run"))
        .expect("test-bench run subcommand exists")
        .render_help()
        .to_string();
    for token in [
        "--process-wasi",
        "testbench-wasi",
        "cargo install harn-cli --features testbench-wasi",
    ] {
        assert!(help.contains(token), "expected `{token}` in help");
    }
}

#[test]
fn bench_portable_rejects_out_of_range_counts_during_parsing() {
    for (flag, value) in [
        ("--iterations", "0"),
        ("--iterations", "1000001"),
        ("--threads", "257"),
        ("--compile-iterations", "100001"),
    ] {
        let error = Cli::try_parse_from([
            "harn",
            "bench",
            "portable",
            "reducer.harn",
            "--input",
            "event.json",
            flag,
            value,
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }
}

#[test]
fn bench_portable_preserves_parent_profile_flag_for_explicit_rejection() {
    let cli = Cli::parse_from([
        "harn",
        "bench",
        "--profile",
        "portable",
        "reducer.harn",
        "--input",
        "event.json",
    ]);
    let Command::Bench(args) = cli.command.unwrap() else {
        panic!("expected bench command");
    };
    assert!(args.profile.text);
}

#[test]
fn profile_environment_aliases_apply_to_supported_commands() {
    let _env = crate::tests::common::harn_state_lock::lock_harn_state();
    struct Restore([(&'static str, Option<String>); 3]);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _restore = Restore([
        ("HARN_PROFILE", std::env::var("HARN_PROFILE").ok()),
        ("HARN_PROFILE_JSON", std::env::var("HARN_PROFILE_JSON").ok()),
        ("HARN_TRACE", std::env::var("HARN_TRACE").ok()),
    ]);
    std::env::set_var("HARN_PROFILE", "1");
    std::env::set_var("HARN_PROFILE_JSON", "env-profile.json");
    std::env::set_var("HARN_TRACE", "1");

    let Command::Run(run) = Cli::parse_from(["harn", "run", "main.harn"])
        .command
        .unwrap()
    else {
        panic!("expected run command");
    };
    assert!(run.trace);
    assert!(run.profile.text);

    let Command::Bench(bench) = Cli::parse_from(["harn", "bench", "main.harn"])
        .command
        .unwrap()
    else {
        panic!("expected bench command");
    };
    assert!(bench.profile.text);

    let Command::Serve(serve) = Cli::parse_from(["harn", "serve", "acp", "agent.harn"])
        .command
        .unwrap()
    else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Acp(acp) = serve.command else {
        panic!("expected serve acp");
    };
    assert!(acp.trace);
    assert!(acp.profile.text);
    assert_eq!(run.profile.json_path, bench.profile.json_path);
    assert_eq!(run.profile.json_path, acp.profile.json_path);
}

#[test]
fn demo_defaults_to_listing_and_replay() {
    let cli = Cli::parse_from(["harn", "demo"]);
    let Command::Demo(list) = cli.command.unwrap() else {
        panic!("expected demo command");
    };
    assert_eq!(list.scenario, None);
    assert!(!list.live);

    let cli = Cli::parse_from(["harn", "demo", "merge-captain"]);
    let Command::Demo(replay) = cli.command.unwrap() else {
        panic!("expected demo command");
    };
    assert!(!replay.live);
}

#[test]
fn demo_live_and_replay_are_mutually_exclusive() {
    assert!(Cli::try_parse_from(["harn", "demo", "merge-captain", "--live", "--replay",]).is_err());
}
