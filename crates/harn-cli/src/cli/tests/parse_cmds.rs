use super::*;
use crate::cli::TimeCommand;

#[test]
fn test_parses_agents_conformance_target_url() {
    let cli = Cli::parse_from([
        "harn",
        "test",
        "agents-conformance",
        "--target",
        "http://localhost:8080",
        "--api-key",
        "test-key",
        "--category",
        "core,streaming",
        "--json",
    ]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert_eq!(args.target.as_deref(), Some("agents-conformance"));
    assert_eq!(args.agents_target.as_deref(), Some("http://localhost:8080"));
    assert_eq!(args.agents_api_key.as_deref(), Some("test-key"));
    assert_eq!(args.agents_category, vec!["core,streaming"]);
    assert!(args.json);
}

#[test]
fn test_run_rejects_deny_allow_conflict() {
    let err = Cli::try_parse_from([
        "harn",
        "run",
        "--deny",
        "read_file",
        "--allow",
        "exec",
        "main.harn",
    ])
    .unwrap_err();

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_parses_dap_subcommand() {
    let cli = Cli::parse_from(["harn", "dap"]);

    let Command::Dap(_) = cli.command.unwrap() else {
        panic!("expected dap command");
    };
}

#[test]
fn test_run_parses_sandbox_roots_and_rejects_no_sandbox_conflict() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--write-root",
        "../receipts",
        "--read-only-root",
        "../shared",
        "--writable-root",
        "/tmp/cache",
        "--read-only-root",
        "/tmp/assets",
        "--allow-process-network",
        "main.harn",
    ]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(
        args.write_root,
        vec![PathBuf::from("../receipts"), PathBuf::from("/tmp/cache")]
    );
    assert_eq!(
        args.read_only_root,
        vec![PathBuf::from("../shared"), PathBuf::from("/tmp/assets")]
    );
    assert!(args.allow_process_network);

    let err = Cli::try_parse_from([
        "harn",
        "run",
        "--no-sandbox",
        "--write-root",
        "../receipts",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

    let err = Cli::try_parse_from([
        "harn",
        "run",
        "--no-sandbox",
        "--allow-process-network",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

    let err = Cli::try_parse_from([
        "harn",
        "run",
        "--no-sandbox",
        "--read-only-root",
        "../shared",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_time_run_parses_sandbox_roots_and_rejects_no_sandbox_conflict() {
    let cli = Cli::parse_from([
        "harn",
        "time",
        "run",
        "--write-root",
        "../receipts",
        "--read-only-root",
        "../shared",
        "--writable-root",
        "/tmp/cache",
        "--allow-process-network",
        "main.harn",
    ]);

    let Command::Time(args) = cli.command.unwrap() else {
        panic!("expected time command");
    };
    let TimeCommand::Run(args) = args.command;
    assert_eq!(
        args.write_root,
        vec![PathBuf::from("../receipts"), PathBuf::from("/tmp/cache")]
    );
    assert_eq!(args.read_only_root, vec![PathBuf::from("../shared")]);
    assert!(args.allow_process_network);

    let err = Cli::try_parse_from([
        "harn",
        "time",
        "run",
        "--no-sandbox",
        "--write-root",
        "../receipts",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

    let err = Cli::try_parse_from([
        "harn",
        "time",
        "run",
        "--no-sandbox",
        "--allow-process-network",
        "main.harn",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn test_parses_run_llm_mock_flags() {
    let cli = Cli::parse_from(["harn", "run", "--llm-mock", "fixtures.jsonl", "main.harn"]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(args.llm_mock.as_deref(), Some("fixtures.jsonl"));
    assert_eq!(args.llm_mock_record, None);

    let cli = Cli::parse_from(["harn", "run", "--llm-mock-record", "out.jsonl", "main.harn"]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(args.llm_mock_record.as_deref(), Some("out.jsonl"));
    assert_eq!(args.llm_mock, None);
}

#[test]
fn test_test_bench_run_help_discloses_wasi_feature_gate() {
    let mut command = Cli::command();
    let help = command
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
        assert!(
            help.contains(token),
            "expected `{token}` in `harn test-bench run --help`, got:\n{help}"
        );
    }
}

#[test]
fn test_parses_run_summary_flags() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--emit-summary-json",
        "--summary-file",
        "summary.jsonl",
        "main.harn",
    ]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.emit_summary_json);
    assert_eq!(
        args.summary_file.as_deref(),
        Some(std::path::Path::new("summary.jsonl"))
    );
    assert_eq!(args.summary_fd, None);

    let cli = Cli::parse_from([
        "harn",
        "run",
        "--emit-summary-json",
        "--summary-fd",
        "3",
        "main.harn",
    ]);
    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(args.summary_fd, Some(3));
}

#[test]
fn test_parses_run_phase_and_rusage_flags() {
    let cli = Cli::parse_from([
        "harn",
        "run",
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
    assert!(args.emit_phase_json);
    assert_eq!(
        args.phase_file.as_deref(),
        Some(std::path::Path::new("phases.jsonl"))
    );
    assert_eq!(args.phase_fd, None);
    assert!(args.emit_rusage_json);
    assert_eq!(args.rusage_file, None);
    assert_eq!(args.rusage_fd, Some(4));
}

#[test]
fn test_parses_eval_tool_calls_args() {
    let cli = Cli::parse_from([
        "harn",
        "eval",
        "tool-calls",
        "--dataset",
        "conformance/tool-call-eval",
        "--planner",
        "provider=mock,model=mock",
        "--binder",
        "mock:mock-binder",
        "--output",
        ".harn-runs/tool-call-eval/latest",
        "--max-cases",
        "3",
    ]);

    let Command::Eval(args) = cli.command.unwrap() else {
        panic!("expected eval command");
    };
    let Some(EvalCommand::ToolCalls(tool_calls)) = args.command else {
        panic!("expected tool-calls command");
    };
    assert_eq!(
        tool_calls.dataset,
        PathBuf::from("conformance/tool-call-eval")
    );
    assert_eq!(
        tool_calls.planner.as_deref(),
        Some("provider=mock,model=mock")
    );
    assert_eq!(tool_calls.binder.as_deref(), Some("mock:mock-binder"));
    assert_eq!(tool_calls.max_cases, Some(3));

    let cli = Cli::parse_from([
        "harn",
        "eval",
        "tool-calls",
        "regression-check",
        "--planner",
        "mock:mock",
        "--against",
        "baseline.json",
        "--max-drop-pp",
        "1.5",
    ]);
    let Command::Eval(args) = cli.command.unwrap() else {
        panic!("expected eval command");
    };
    let Some(EvalCommand::ToolCalls(tool_calls)) = args.command else {
        panic!("expected tool-calls command");
    };
    let Some(EvalToolCallsCommand::RegressionCheck(regression)) = tool_calls.command else {
        panic!("expected regression-check command");
    };
    assert_eq!(regression.planner.as_deref(), Some("mock:mock"));
    assert_eq!(regression.against, PathBuf::from("baseline.json"));
    assert_eq!(regression.max_drop_pp, 1.5);
}

#[test]
fn test_parses_eval_context_args() {
    let cli = Cli::parse_from([
        "harn",
        "eval",
        "context",
        "examples/evals/context-engineering-smoke.json",
        "--output",
        ".harn-runs/context-eval/smoke",
        "--json",
    ]);

    let Command::Eval(args) = cli.command.unwrap() else {
        panic!("expected eval command");
    };
    let Some(EvalCommand::Context(context)) = args.command else {
        panic!("expected context command");
    };
    assert_eq!(
        context.manifest,
        PathBuf::from("examples/evals/context-engineering-smoke.json")
    );
    assert_eq!(
        context.output,
        Some(PathBuf::from(".harn-runs/context-eval/smoke"))
    );
    assert!(context.json);
}

#[test]
fn test_parses_eval_skill_gate_args() {
    let cli = Cli::parse_from([
        "harn",
        "eval",
        "skill-gate",
        "examples/evals/skill-gate/smoke/manifest.json",
        "--output",
        ".harn-runs/skill-gate/smoke",
        "--json",
    ]);

    let Command::Eval(args) = cli.command.unwrap() else {
        panic!("expected eval command");
    };
    let Some(EvalCommand::SkillGate(skill_gate)) = args.command else {
        panic!("expected skill-gate command");
    };
    assert_eq!(
        skill_gate.manifest,
        PathBuf::from("examples/evals/skill-gate/smoke/manifest.json")
    );
    assert_eq!(
        skill_gate.output,
        Some(PathBuf::from(".harn-runs/skill-gate/smoke"))
    );
    assert!(skill_gate.json);
}

#[test]
fn test_parses_eval_scope_triage_args() {
    let cli = Cli::parse_from([
        "harn",
        "eval",
        "scope_triage",
        "--dataset",
        "evals/scope_triage/dataset.json",
        "--output",
        ".harn-runs/scope-triage/smoke",
        "--max-cases",
        "5",
        "--confidence-threshold",
        "0.8",
        "--live",
        "--json",
    ]);

    let Command::Eval(args) = cli.command.unwrap() else {
        panic!("expected eval command");
    };
    let Some(EvalCommand::ScopeTriage(scope)) = args.command else {
        panic!("expected scope_triage command");
    };
    assert_eq!(
        scope.dataset,
        PathBuf::from("evals/scope_triage/dataset.json")
    );
    assert_eq!(
        scope.output,
        Some(PathBuf::from(".harn-runs/scope-triage/smoke"))
    );
    assert_eq!(scope.max_cases, Some(5));
    assert_eq!(scope.confidence_threshold, 0.8);
    assert!(scope.live);
    assert!(scope.json);
}

#[test]
fn test_parses_run_yes_flag() {
    let cli = Cli::parse_from(["harn", "run", "--yes", "main.harn"]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.yes);
}

#[test]
fn test_parses_run_explain_cost_flag() {
    let cli = Cli::parse_from(["harn", "run", "--explain-cost", "main.harn"]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.explain_cost);
    assert_eq!(args.file.as_deref(), Some("main.harn"));
}

#[test]
fn test_parses_run_attestation_flags() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--attest",
        "--receipt-out",
        "receipt.json",
        "--attest-agent",
        "agent-1",
        "main.harn",
    ]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(args.attest);
    assert_eq!(args.receipt_out.as_deref(), Some("receipt.json"));
    assert_eq!(args.attest_agent.as_deref(), Some("agent-1"));
}

#[test]
fn test_parses_verify_receipt() {
    let cli = Cli::parse_from(["harn", "verify", "receipt.json", "--json"]);

    let Command::Verify(args) = cli.command.unwrap() else {
        panic!("expected verify command");
    };
    assert_eq!(args.receipt, "receipt.json");
    assert!(args.json);
}

#[test]
fn test_parses_pipeline_lab_template() {
    let cli = Cli::parse_from([
        "harn",
        "new",
        "pipeline-lab-demo",
        "--template",
        "pipeline-lab",
    ]);

    let Command::New(args) = cli.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(args.template, Some(ProjectTemplate::PipelineLab));
}

#[test]
fn test_parses_chat_template() {
    let cli = Cli::parse_from(["harn", "new", "chat-demo", "--template", "chat"]);

    let Command::New(args) = cli.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(args.first.as_deref(), Some("chat-demo"));
    assert_eq!(args.template, Some(ProjectTemplate::Chat));
}

#[test]
fn test_parses_playground_args() {
    let cli = Cli::parse_from([
        "harn",
        "playground",
        "--host",
        "examples/playground/host.harn",
        "--script",
        "examples/playground/echo.harn",
        "--task",
        "hi",
        "--llm",
        "ollama:qwen2.5-coder:latest",
        "--yes",
        "--watch",
    ]);

    let Command::Playground(args) = cli.command.unwrap() else {
        panic!("expected playground command");
    };
    assert_eq!(args.host, "examples/playground/host.harn");
    assert_eq!(args.script, "examples/playground/echo.harn");
    assert_eq!(args.task.as_deref(), Some("hi"));
    assert_eq!(args.llm.as_deref(), Some("ollama:qwen2.5-coder:latest"));
    assert_eq!(args.llm_mock, None);
    assert_eq!(args.llm_mock_record, None);
    assert!(args.yes);
    assert!(args.watch);
}

#[test]
fn test_parses_try_command() {
    let cli = Cli::parse_from([
        "harn",
        "try",
        "hi",
        "--max-iterations",
        "7",
        "--tool-format",
        "text",
        "--override-reason",
        "compare native drift",
    ]);

    let Command::Try(args) = cli.command.unwrap() else {
        panic!("expected try command");
    };
    assert_eq!(args.prompt, "hi");
    assert_eq!(args.max_iterations, 7);
    assert_eq!(args.tool_format.as_deref(), Some("text"));
    assert_eq!(
        args.override_reason.as_deref(),
        Some("compare native drift")
    );
}

#[test]
fn test_parses_playground_llm_mock_flags() {
    let cli = Cli::parse_from([
        "harn",
        "playground",
        "--llm-mock",
        "fixtures.jsonl",
        "--host",
        "host.harn",
    ]);

    let Command::Playground(args) = cli.command.unwrap() else {
        panic!("expected playground command");
    };
    assert_eq!(args.llm_mock.as_deref(), Some("fixtures.jsonl"));
    assert_eq!(args.llm_mock_record, None);

    let cli = Cli::parse_from(["harn", "playground", "--llm-mock-record", "recorded.jsonl"]);

    let Command::Playground(args) = cli.command.unwrap() else {
        panic!("expected playground command");
    };
    assert_eq!(args.llm_mock, None);
    assert_eq!(args.llm_mock_record.as_deref(), Some("recorded.jsonl"));
}

#[test]
fn test_parses_bench_args() {
    let cli = Cli::parse_from([
        "harn",
        "bench",
        "main.harn",
        "--iterations",
        "25",
        "--profile",
        "--profile-json",
        "bench.json",
    ]);

    let Command::Bench(args) = cli.command.unwrap() else {
        panic!("expected bench command");
    };
    assert_eq!(args.file.as_deref(), Some("main.harn"));
    assert_eq!(args.iterations, 25);
    assert!(args.profile.text);
    assert_eq!(
        args.profile.json_path.as_deref(),
        Some(std::path::Path::new("bench.json"))
    );
}

#[test]
fn test_parses_bench_replay_args() {
    let cli = Cli::parse_from([
        "harn",
        "bench",
        "replay",
        "benchmarks/replay/suite.json",
        "--json",
        "--output",
        "replay-benchmark.json",
        "--filter",
        "permission",
        "--adapter",
        "opencode-jsonl",
        "--external-first",
        "first.jsonl",
        "--external-second",
        "second.jsonl",
        "--external-name",
        "opencode-permission",
    ]);

    let Command::Bench(args) = cli.command.unwrap() else {
        panic!("expected bench command");
    };
    let Some(crate::cli::BenchCommand::Replay(replay)) = args.command else {
        panic!("expected bench replay command");
    };
    assert_eq!(
        replay.selection.as_deref(),
        Some(std::path::Path::new("benchmarks/replay/suite.json"))
    );
    assert!(replay.json);
    assert_eq!(
        replay.output.as_deref(),
        Some(std::path::Path::new("replay-benchmark.json"))
    );
    assert_eq!(replay.filter.as_deref(), Some("permission"));
    assert_eq!(replay.adapter.as_deref(), Some("opencode-jsonl"));
    assert_eq!(
        replay.external_first.as_deref(),
        Some(std::path::Path::new("first.jsonl"))
    );
    assert_eq!(
        replay.external_second.as_deref(),
        Some(std::path::Path::new("second.jsonl"))
    );
    assert_eq!(replay.external_name, "opencode-permission");
}

#[test]
fn test_profile_env_aliases_apply_to_supported_commands() {
    let _env = crate::tests::common::env_lock::lock_env().blocking_lock();
    struct EnvRestore {
        saved: [(&'static str, Option<String>); 3],
    }
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in self.saved.iter() {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _restore = EnvRestore {
        saved: [
            ("HARN_PROFILE", std::env::var("HARN_PROFILE").ok()),
            ("HARN_PROFILE_JSON", std::env::var("HARN_PROFILE_JSON").ok()),
            ("HARN_TRACE", std::env::var("HARN_TRACE").ok()),
        ],
    };
    std::env::set_var("HARN_PROFILE", "1");
    std::env::set_var("HARN_PROFILE_JSON", "env-profile.json");
    std::env::set_var("HARN_TRACE", "1");

    let run = Cli::parse_from(["harn", "run", "main.harn"]);
    let Command::Run(run_args) = run.command.unwrap() else {
        panic!("expected run command");
    };
    assert!(run_args.trace);
    assert!(run_args.profile.text);
    assert_eq!(
        run_args.profile.json_path.as_deref(),
        Some(std::path::Path::new("env-profile.json"))
    );

    let bench = Cli::parse_from(["harn", "bench", "main.harn"]);
    let Command::Bench(bench_args) = bench.command.unwrap() else {
        panic!("expected bench command");
    };
    assert!(bench_args.profile.text);
    assert_eq!(
        bench_args.profile.json_path.as_deref(),
        Some(std::path::Path::new("env-profile.json"))
    );

    let serve = Cli::parse_from(["harn", "serve", "acp", "agent.harn"]);
    let Command::Serve(serve_args) = serve.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Acp(acp_args) = serve_args.command else {
        panic!("expected serve acp");
    };
    assert!(acp_args.trace);
    assert!(acp_args.profile.text);
    assert_eq!(
        acp_args.profile.json_path.as_deref(),
        Some(std::path::Path::new("env-profile.json"))
    );
}

#[test]
fn test_parses_quickstart_args() {
    let cli = Cli::parse_from([
        "harn",
        "quickstart",
        "--non-interactive",
        "--provider",
        "ollama",
        "--model",
        "qwen2.5-coder:latest",
    ]);

    let Command::Quickstart(args) = cli.command.unwrap() else {
        panic!("expected quickstart command");
    };
    assert!(args.non_interactive);
    assert_eq!(args.provider.as_deref(), Some("ollama"));
    assert_eq!(args.model.as_deref(), Some("qwen2.5-coder:latest"));
}

#[test]
fn test_parses_demo_no_args_lists_scenarios() {
    let cli = Cli::parse_from(["harn", "demo"]);
    let Command::Demo(args) = cli.command.unwrap() else {
        panic!("expected demo command");
    };
    assert_eq!(args.scenario, None);
    assert!(!args.list);
    assert!(!args.live);
    assert!(!args.replay);
    assert!(!args.json);
}

#[test]
fn test_parses_demo_named_scenario_replay_default() {
    let cli = Cli::parse_from(["harn", "demo", "merge-captain"]);
    let Command::Demo(args) = cli.command.unwrap() else {
        panic!("expected demo command");
    };
    assert_eq!(args.scenario.as_deref(), Some("merge-captain"));
    assert!(!args.live);
}

#[test]
fn test_parses_demo_live_and_json_flags() {
    let cli = Cli::parse_from([
        "harn",
        "demo",
        "provider-race",
        "--live",
        "--json",
        "--no-record",
    ]);
    let Command::Demo(args) = cli.command.unwrap() else {
        panic!("expected demo command");
    };
    assert_eq!(args.scenario.as_deref(), Some("provider-race"));
    assert!(args.live);
    assert!(args.json);
    assert!(args.no_record);
}

#[test]
fn test_parses_demo_live_and_replay_are_mutually_exclusive() {
    let result = Cli::try_parse_from(["harn", "demo", "merge-captain", "--live", "--replay"]);
    assert!(
        result.is_err(),
        "--live and --replay must be mutually exclusive"
    );
}
