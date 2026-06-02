use std::path::PathBuf;
use std::time::Duration as StdDuration;

use super::{
    CheckOutputFormat, Cli, Command, CompletionShell, ConfigCommand, ConnectCommand,
    ConnectorCommand, CrystallizeCommand, EvalCommand, EvalToolCallsCommand, FlowArchivistCommand,
    FlowCommand, HarnessThreadingMode, LocalCommand, McpCommand, McpMockCommand, ModelsCommand,
    OrchestratorCommand, OrchestratorDeployProvider, OrchestratorLogFormat,
    OrchestratorQueueCommand, OrchestratorTenantCommand, PackageArtifactsCommand,
    PackageCacheCommand, PackageCommand, PackageScaffoldCommand, PersonaCommand, ProjectTemplate,
    ProviderCapabilitiesCommand, ProviderCommand, ProviderToolProbeModeArg, ProvidersCommand,
    PublishArgs, RuleCommand, RunsCommand, SessionCommand, SkillCommand, SkillKeyCommand,
    SkillTrustCommand, SkillsCommand, ToolCommand, TraceCommand, TriggerCommand, TrustCommand,
    TrustOutcomeArg, TrustTierArg,
};
use clap::{CommandFactory, Parser};

#[test]
fn test_parses_conformance_target_selection() {
    let cli = Cli::parse_from([
        "harn",
        "test",
        "conformance",
        "tests/worktree_runtime.harn",
        "--verbose",
        "--differential-optimizations",
    ]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert_eq!(args.target.as_deref(), Some("conformance"));
    assert_eq!(
        args.selection.as_deref(),
        Some("tests/worktree_runtime.harn")
    );
    assert!(args.verbose);
    assert!(args.differential_optimizations);
}

#[test]
fn test_parses_config_inspect_explain() {
    let cli = Cli::parse_from([
        "harn",
        "config",
        "inspect",
        "--explain",
        "--config",
        "local.toml",
        "--managed",
        "managed.toml",
    ]);

    let Command::Config(args) = cli.command.unwrap() else {
        panic!("expected config command");
    };
    let ConfigCommand::Inspect(inspect) = args.command else {
        panic!("expected inspect command");
    };
    assert!(inspect.explain);
    assert_eq!(inspect.config_files, vec![PathBuf::from("local.toml")]);
    assert_eq!(inspect.managed_files, vec![PathBuf::from("managed.toml")]);
}

#[test]
fn test_parses_config_validate_and_schema() {
    let cli = Cli::parse_from(["harn", "config", "validate", "--managed", "policy.toml"]);
    let Command::Config(args) = cli.command.unwrap() else {
        panic!("expected config command");
    };
    let ConfigCommand::Validate(validate) = args.command else {
        panic!("expected validate command");
    };
    assert!(validate.managed);
    assert_eq!(validate.files, vec![PathBuf::from("policy.toml")]);

    let cli = Cli::parse_from(["harn", "config", "schema", "--output", "schema.json"]);
    let Command::Config(args) = cli.command.unwrap() else {
        panic!("expected config command");
    };
    let ConfigCommand::Schema(schema) = args.command else {
        panic!("expected schema command");
    };
    assert_eq!(schema.output, Some(PathBuf::from("schema.json")));
}

#[test]
fn test_parses_provider_capabilities_audit_json() {
    let cli = Cli::parse_from(["harn", "provider", "capabilities", "audit", "--json"]);

    let Command::Provider(args) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Capabilities(capabilities) = args.command;
    let ProviderCapabilitiesCommand::Audit(audit) = capabilities.command else {
        panic!("expected audit command");
    };
    assert!(audit.json);
}

#[test]
fn test_parses_provider_capabilities_promote_from_eval() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "capabilities",
        "promote-from-eval",
        "overlay.toml",
        "--catalog",
        "custom-capabilities.toml",
    ]);

    let Command::Provider(args) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Capabilities(capabilities) = args.command;
    let ProviderCapabilitiesCommand::PromoteFromEval(promote) = capabilities.command else {
        panic!("expected promote-from-eval command");
    };
    assert_eq!(promote.overlay_path, PathBuf::from("overlay.toml"));
    assert_eq!(promote.catalog, PathBuf::from("custom-capabilities.toml"));
}

#[test]
fn test_parses_routes_json() {
    let cli = Cli::parse_from(["harn", "routes", "fixtures/project", "--json"]);

    let Command::Routes(args) = cli.command.unwrap() else {
        panic!("expected routes command");
    };
    assert_eq!(args.root, PathBuf::from("fixtures/project"));
    assert!(args.json);
}

#[test]
fn test_parses_graph_json_and_module_filter() {
    let cli = Cli::parse_from([
        "harn",
        "graph",
        "fixtures/project",
        "--json",
        "--module",
        "main",
    ]);

    let Command::Graph(args) = cli.command.unwrap() else {
        panic!("expected graph command");
    };
    assert_eq!(args.root, PathBuf::from("fixtures/project"));
    assert!(args.json);
    assert_eq!(args.module.as_deref(), Some("main"));
}

#[test]
fn test_parses_parse_and_tokens_json() {
    let parse = Cli::parse_from(["harn", "parse", "main.harn", "--json"]);
    let Command::Parse(parse_args) = parse.command.unwrap() else {
        panic!("expected parse command");
    };
    assert_eq!(parse_args.path, "main.harn");
    assert!(parse_args.json);

    let tokens = Cli::parse_from(["harn", "tokens", "main.harn", "--json"]);
    let Command::Tokens(tokens_args) = tokens.command.unwrap() else {
        panic!("expected tokens command");
    };
    assert_eq!(tokens_args.path, "main.harn");
    assert!(tokens_args.json);
}

#[test]
fn test_parses_fix_plan_json_args() {
    let cli = Cli::parse_from([
        "harn",
        "fix",
        "--plan",
        "--json",
        "--safety",
        "behavior-preserving",
        "main.harn",
    ]);

    let Command::Fix(args) = cli.command.unwrap() else {
        panic!("expected fix command");
    };
    assert!(args.plan);
    assert!(args.json);
    assert_eq!(
        args.safety.map(|safety| safety.as_str()),
        Some("behavior-preserving")
    );
    assert_eq!(args.harness_threading, HarnessThreadingMode::LocalGlobal);
    assert_eq!(args.path, PathBuf::from("main.harn"));
}

#[test]
fn test_parses_fix_apply_dry_run_args() {
    let cli = Cli::parse_from([
        "harn",
        "fix",
        "--apply",
        "--dry-run",
        "--json",
        "--safety",
        "scope-local",
        "--harness-threading",
        "thread-params",
        "src/",
    ]);

    let Command::Fix(args) = cli.command.unwrap() else {
        panic!("expected fix command");
    };
    assert!(args.apply);
    assert!(args.dry_run);
    assert!(args.json);
    assert_eq!(
        args.safety.map(|safety| safety.as_str()),
        Some("scope-local")
    );
    assert_eq!(args.harness_threading, HarnessThreadingMode::ThreadParams);
    assert_eq!(args.path, PathBuf::from("src/"));
}

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
fn test_run_parses_read_only_roots_and_rejects_no_sandbox_conflict() {
    let cli = Cli::parse_from([
        "harn",
        "run",
        "--read-only-root",
        "../shared",
        "--read-only-root",
        "/tmp/assets",
        "main.harn",
    ]);

    let Command::Run(args) = cli.command.unwrap() else {
        panic!("expected run command");
    };
    assert_eq!(
        args.read_only_root,
        vec![PathBuf::from("../shared"), PathBuf::from("/tmp/assets")]
    );

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
fn test_parses_mcp_login_flags() {
    let cli = Cli::parse_from([
        "harn",
        "mcp",
        "login",
        "notion",
        "--url",
        "https://example.com/mcp",
        "--client-id",
        "abc",
    ]);

    let Command::Mcp(args) = cli.command.unwrap() else {
        panic!("expected mcp command");
    };
    let McpCommand::Login(login) = args.command else {
        panic!("expected mcp login");
    };
    assert_eq!(login.target.as_deref(), Some("notion"));
    assert_eq!(login.url.as_deref(), Some("https://example.com/mcp"));
    assert_eq!(login.client_id.as_deref(), Some("abc"));
}

#[test]
fn test_parses_connect_oauth_flags() {
    let cli = Cli::parse_from([
        "harn",
        "connect",
        "slack",
        "--client-id",
        "client",
        "--client-secret",
        "secret",
        "--scope",
        "chat:write app_mentions:read",
        "--no-open",
    ]);

    let Command::Connect(args) = cli.command.unwrap() else {
        panic!("expected connect command");
    };
    let Some(ConnectCommand::Slack(slack)) = args.command else {
        panic!("expected connect slack");
    };
    assert_eq!(slack.client_id.as_deref(), Some("client"));
    assert_eq!(slack.client_secret.as_deref(), Some("secret"));
    assert_eq!(slack.scope.as_deref(), Some("chat:write app_mentions:read"));
    assert!(slack.no_open);

    let cli = Cli::parse_from([
        "harn",
        "connect",
        "linear",
        "--client-id",
        "linear-client",
        "--client-secret",
        "linear-secret",
    ]);
    let Command::Connect(args) = cli.command.unwrap() else {
        panic!("expected connect command");
    };
    let Some(ConnectCommand::Linear(linear)) = args.command else {
        panic!("expected connect linear");
    };
    assert!(linear.url.is_none());
    assert_eq!(linear.client_id.as_deref(), Some("linear-client"));

    let cli = Cli::parse_from([
        "harn",
        "connect",
        "acme",
        "--client-id",
        "acme-client",
        "--scope",
        "tickets.read",
        "--no-open",
    ]);
    let Command::Connect(args) = cli.command.unwrap() else {
        panic!("expected connect command");
    };
    let Some(ConnectCommand::Provider(raw)) = args.command else {
        panic!("expected external provider connect command");
    };
    assert_eq!(
        raw,
        vec![
            "acme".to_string(),
            "--client-id".to_string(),
            "acme-client".to_string(),
            "--scope".to_string(),
            "tickets.read".to_string(),
            "--no-open".to_string()
        ]
    );
}

#[test]
fn test_parses_connect_management_flags() {
    let cli = Cli::parse_from(["harn", "connect", "--list", "--json"]);

    let Command::Connect(args) = cli.command.unwrap() else {
        panic!("expected connect command");
    };
    assert!(args.list);
    assert!(args.json);
    assert!(args.command.is_none());

    let cli = Cli::parse_from([
        "harn",
        "connect",
        "--generic",
        "acme",
        "https://mcp.example.com/mcp",
    ]);
    let Command::Connect(args) = cli.command.unwrap() else {
        panic!("expected connect command");
    };
    assert_eq!(args.generic, vec!["acme", "https://mcp.example.com/mcp"]);
}

#[test]
fn test_parses_mcp_serve_flags() {
    let cli = Cli::parse_from([
        "harn",
        "mcp",
        "serve",
        "--config",
        "workspace/harn.toml",
        "--state-dir",
        "state/orchestrator",
        "--transport",
        "http",
        "--bind",
        "127.0.0.1:9000",
        "--path",
        "/rpc",
        "--sse-path",
        "/events",
        "--messages-path",
        "/legacy/messages",
    ]);

    let Command::Mcp(args) = cli.command.unwrap() else {
        panic!("expected mcp command");
    };
    let McpCommand::Serve(serve) = args.command else {
        panic!("expected mcp serve");
    };
    assert_eq!(serve.local.config, PathBuf::from("workspace/harn.toml"));
    assert_eq!(serve.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(serve.transport, crate::cli::McpServeTransport::Http);
    assert_eq!(serve.bind.to_string(), "127.0.0.1:9000");
    assert_eq!(serve.path, "/rpc");
    assert_eq!(serve.sse_path, "/events");
    assert_eq!(serve.messages_path, "/legacy/messages");
}

#[test]
fn test_parses_mcp_mock_commands() {
    let cli = Cli::parse_from([
        "harn",
        "mcp",
        "mock",
        "record",
        "--cassette",
        "cassette.json",
        "--",
        "python3",
        "server.py",
    ]);
    let Command::Mcp(args) = cli.command.unwrap() else {
        panic!("expected mcp command");
    };
    let McpCommand::Mock(mock) = args.command else {
        panic!("expected mcp mock");
    };
    let McpMockCommand::Record(record) = mock.command else {
        panic!("expected mcp mock record");
    };
    assert_eq!(record.cassette, "cassette.json");
    assert_eq!(record.command, vec!["python3", "server.py"]);

    let cli = Cli::parse_from([
        "harn",
        "mcp",
        "mock",
        "eval",
        "--spec",
        "world.json",
        "--state",
        "run1.json",
        "--state",
        "run2.json",
    ]);
    let Command::Mcp(args) = cli.command.unwrap() else {
        panic!("expected mcp command");
    };
    let McpCommand::Mock(mock) = args.command else {
        panic!("expected mcp mock");
    };
    let McpMockCommand::Eval(eval) = mock.command else {
        panic!("expected mcp mock eval");
    };
    assert_eq!(eval.spec, "world.json");
    assert_eq!(eval.states, vec!["run1.json", "run2.json"]);
}

#[test]
fn test_parses_serve_mcp_flags() {
    let cli = Cli::parse_from([
        "harn",
        "serve",
        "mcp",
        "--transport",
        "http",
        "--bind",
        "127.0.0.1:9001",
        "--path",
        "/rpc",
        "--sse-path",
        "/events",
        "--messages-path",
        "/legacy/messages",
        "--api-key",
        "alpha,beta",
        "--hmac-secret",
        "shared",
        "--tls",
        "pem",
        "--cert",
        "tls/cert.pem",
        "--key",
        "tls/key.pem",
        "server.harn",
    ]);

    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Mcp(serve) = args.command else {
        panic!("expected serve mcp");
    };
    assert_eq!(serve.transport, crate::cli::McpServeTransport::Http);
    assert_eq!(serve.bind.to_string(), "127.0.0.1:9001");
    assert_eq!(serve.path, "/rpc");
    assert_eq!(serve.sse_path, "/events");
    assert_eq!(serve.messages_path, "/legacy/messages");
    assert_eq!(serve.api_key, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(serve.hmac_secret.as_deref(), Some("shared"));
    assert_eq!(serve.tls, crate::cli::ServeTlsMode::Pem);
    assert_eq!(serve.cert, Some(PathBuf::from("tls/cert.pem")));
    assert_eq!(serve.key, Some(PathBuf::from("tls/key.pem")));
    assert_eq!(serve.file, "server.harn");
}

#[test]
fn test_parses_serve_acp() {
    let cli = Cli::parse_from([
        "harn",
        "serve",
        "acp",
        "--api-key",
        "alpha,beta",
        "--hmac-secret",
        "shared",
        "--trace",
        "--profile",
        "--profile-json",
        "profiles/acp.ndjson",
        "agent.harn",
    ]);

    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Acp(serve) = args.command else {
        panic!("expected serve acp");
    };
    assert_eq!(serve.api_key, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(serve.hmac_secret.as_deref(), Some("shared"));
    assert!(serve.trace);
    assert!(serve.profile.text);
    assert_eq!(
        serve.profile.json_path.as_deref(),
        Some(std::path::Path::new("profiles/acp.ndjson"))
    );
    assert_eq!(serve.file.as_deref(), Some("agent.harn"));
}

#[test]
fn test_parses_serve_acp_without_file_for_attach_mode() {
    // The ACP registry launches the bare `harn serve acp` with no
    // positional file; the parse must succeed and leave `file` unset so
    // the command boots the file-less attach server.
    let cli = Cli::parse_from(["harn", "serve", "acp"]);
    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Acp(serve) = args.command else {
        panic!("expected serve acp");
    };
    assert_eq!(serve.file, None);
}

#[test]
fn test_parses_serve_a2a_bind_and_auth() {
    let cli = Cli::parse_from([
        "harn",
        "serve",
        "a2a",
        "--bind",
        "0.0.0.0:3000",
        "--api-key",
        "alpha,beta",
        "--hmac-secret",
        "shared",
        "agent.harn",
    ]);

    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::A2a(serve) = args.command else {
        panic!("expected serve a2a");
    };
    assert_eq!(
        serve.bind.map(|addr| addr.to_string()).as_deref(),
        Some("0.0.0.0:3000")
    );
    assert_eq!(serve.port, 8080);
    assert_eq!(serve.api_key, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(serve.hmac_secret.as_deref(), Some("shared"));
    assert_eq!(serve.file, "agent.harn");
}

#[test]
fn test_parses_serve_api() {
    let cli = Cli::parse_from([
        "harn",
        "serve",
        "api",
        "--bind",
        "127.0.0.1:9898",
        "--public-url",
        "https://agent.example.test",
        "--api-key",
        "alpha,beta",
        "--hmac-secret",
        "shared",
        "--tls",
        "pem",
        "--cert",
        "tls/cert.pem",
        "--key",
        "tls/key.pem",
        "--trace",
        "--profile-json",
        "profiles/api.ndjson",
        "agent.harn",
    ]);

    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::Api(serve) = args.command else {
        panic!("expected serve api");
    };
    assert_eq!(serve.bind.to_string(), "127.0.0.1:9898");
    assert_eq!(
        serve.public_url.as_deref(),
        Some("https://agent.example.test")
    );
    assert_eq!(serve.api_key, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(serve.hmac_secret.as_deref(), Some("shared"));
    assert_eq!(serve.tls, crate::cli::ServeTlsMode::Pem);
    assert_eq!(serve.cert, Some(PathBuf::from("tls/cert.pem")));
    assert_eq!(serve.key, Some(PathBuf::from("tls/key.pem")));
    assert!(serve.trace);
    assert_eq!(
        serve.profile.json_path.as_deref(),
        Some(std::path::Path::new("profiles/api.ndjson"))
    );
    assert_eq!(serve.file, "agent.harn");
}

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
    let RunsCommand::Inspect(inspect) = args.command;
    assert_eq!(inspect.path, "run.json");
    assert_eq!(inspect.compare.as_deref(), Some("baseline.json"));
}

#[test]
fn test_parses_replay_sources_and_runs() {
    let cli = Cli::parse_from(["harn", "replay", "run.json", "--json"]);
    let Command::Replay(replay) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert_eq!(replay.path.as_deref(), Some("run.json"));
    assert!(replay.fixture.is_none());
    assert!(replay.session_id.is_none());
    assert_eq!(replay.runs, 1);
    assert!(replay.json);

    let cli = Cli::parse_from([
        "harn",
        "replay",
        "--fixture",
        "trace.json",
        "--runs",
        "3",
        "--json",
    ]);
    let Command::Replay(replay) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert!(replay.path.is_none());
    assert_eq!(replay.fixture.as_deref(), Some("trace.json"));
    assert_eq!(replay.runs, 3);
    assert!(replay.json);

    let cli = Cli::parse_from([
        "harn",
        "replay",
        "--session-id",
        "session-123",
        "--events-db",
        ".harn/events.sqlite",
        "--runs",
        "2",
    ]);
    let Command::Replay(replay) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert_eq!(replay.session_id.as_deref(), Some("session-123"));
    assert_eq!(replay.events_db.as_deref(), Some(".harn/events.sqlite"));
    assert_eq!(replay.runs, 2);
    assert!(replay.counterfactual.is_empty());

    let cli = Cli::parse_from([
        "harn",
        "replay",
        "--session-id",
        "session-123",
        "--events-db",
        ".harn/events.sqlite",
        "--at",
        "7",
        "--counterfactual",
        "first.harn",
        "--counterfactual",
        "second.harn",
    ]);
    let Command::Replay(replay) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert_eq!(replay.at, Some(7));
    assert_eq!(replay.counterfactual, vec!["first.harn", "second.harn"]);
}

#[test]
fn test_parses_session_bundle_commands() {
    let cli = Cli::parse_from([
        "harn",
        "session",
        "export",
        "run.json",
        "--out",
        "bundle.json",
        "--include-attachments",
    ]);

    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Export(export) = args.command else {
        panic!("expected session export");
    };
    assert_eq!(export.run_record, "run.json");
    assert_eq!(export.out.as_deref(), Some("bundle.json"));
    assert!(export.include_attachments);

    let cli = Cli::parse_from([
        "harn",
        "session",
        "import",
        "bundle.json",
        "--out",
        "imported.json",
        "--allow-unsafe-secret-markers",
    ]);

    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Import(import) = args.command else {
        panic!("expected session import");
    };
    assert_eq!(import.bundle, "bundle.json");
    assert_eq!(import.out.as_deref(), Some("imported.json"));
    assert!(import.allow_unsafe_secret_markers);

    let cli = Cli::parse_from(["harn", "session", "validate", "bundle.json", "--json"]);
    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Validate(validate) = args.command else {
        panic!("expected session validate");
    };
    assert_eq!(validate.bundle, "bundle.json");
    assert!(validate.json);

    let cli = Cli::parse_from(["harn", "session", "schema", "--check"]);
    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Schema(schema) = args.command else {
        panic!("expected session schema");
    };
    assert!(schema.check);
}

#[test]
fn test_parses_trace_import_args() {
    let cli = Cli::parse_from([
        "harn",
        "trace",
        "import",
        "--trace-file",
        "langfuse.jsonl",
        "--trace-id",
        "trace_123",
        "--output",
        "fixtures/imported.jsonl",
    ]);

    let Command::Trace(args) = cli.command.unwrap() else {
        panic!("expected trace command");
    };
    let TraceCommand::Import(import) = args.command;
    assert_eq!(import.trace_file, "langfuse.jsonl");
    assert_eq!(import.trace_id.as_deref(), Some("trace_123"));
    assert_eq!(import.output, "fixtures/imported.jsonl");
}

#[test]
fn test_parses_crystallize_args() {
    let cli = Cli::parse_from([
        "harn",
        "crystallize",
        "--from",
        "fixtures/crystallize",
        "--out",
        "workflows/version_bump.harn",
        "--report",
        "reports/version_bump.json",
        "--eval-pack",
        "harn.eval.toml",
        "--min-examples",
        "5",
        "--shadow-from",
        "fixtures/crystallize/holdout",
        "--promotion-min-confidence",
        "0.9",
        "--workflow-name",
        "version_bump",
    ]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    assert!(args.command.is_none());
    assert_eq!(args.from.as_deref(), Some("fixtures/crystallize"));
    assert_eq!(args.out.as_deref(), Some("workflows/version_bump.harn"));
    assert_eq!(args.report.as_deref(), Some("reports/version_bump.json"));
    assert_eq!(args.eval_pack.as_deref(), Some("harn.eval.toml"));
    assert_eq!(args.min_examples, 5);
    assert_eq!(
        args.shadow_from,
        vec!["fixtures/crystallize/holdout".to_string()]
    );
    assert_eq!(args.promotion_min_confidence, 0.9);
    assert_eq!(args.workflow_name.as_deref(), Some("version_bump"));
}

#[test]
fn test_parses_crystallize_validate_subcommand() {
    let cli = Cli::parse_from(["harn", "crystallize", "validate", "bundles/version-bump"]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    let Some(CrystallizeCommand::Validate(validate)) = args.command else {
        panic!("expected validate subcommand");
    };
    assert_eq!(validate.bundle_dir, "bundles/version-bump");
}

#[test]
fn test_parses_crystallize_bundle_flag() {
    let cli = Cli::parse_from([
        "harn",
        "crystallize",
        "--from",
        "fixtures/crystallize",
        "--out",
        "workflows/version_bump.harn",
        "--report",
        "reports/version_bump.json",
        "--bundle",
        "bundles/version-bump",
        "--bundle-team",
        "platform",
        "--bundle-risk-level",
        "medium",
    ]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    assert!(args.command.is_none());
    assert_eq!(args.bundle.as_deref(), Some("bundles/version-bump"));
    assert_eq!(args.bundle_team.as_deref(), Some("platform"));
    assert_eq!(args.bundle_risk_level.as_deref(), Some("medium"));
}

#[test]
fn test_parses_package_evals_flag() {
    let cli = Cli::parse_from(["harn", "test", "package", "--evals"]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert_eq!(args.target.as_deref(), Some("package"));
    assert!(args.evals);
}

#[test]
fn test_parses_merge_captain_ladder_args() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "ladder",
        "personas/merge_captain/harn.eval.toml",
        "--report-out",
        "ladder-report.json",
        "--format",
        "json",
    ]);

    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let super::MergeCaptainCommand::Ladder(ladder) = args.command else {
        panic!("expected merge-captain ladder command");
    };
    assert_eq!(ladder.manifest, "personas/merge_captain/harn.eval.toml");
    assert_eq!(ladder.report_out.as_deref(), Some("ladder-report.json"));
    assert!(!ladder.json);
    assert!(matches!(
        ladder.format,
        crate::cli::MergeCaptainLadderFormat::Json
    ));
}

#[test]
fn test_parses_merge_captain_ladder_json_alias() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "ladder",
        "personas/merge_captain/harn.eval.toml",
        "--json",
    ]);

    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let super::MergeCaptainCommand::Ladder(ladder) = args.command else {
        panic!("expected merge-captain ladder command");
    };
    assert!(ladder.json);
    assert!(matches!(
        ladder.format,
        crate::cli::MergeCaptainLadderFormat::Text
    ));
}

#[test]
fn test_parses_merge_captain_iterate_args() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "iterate",
        "examples/personas/merge_captain/iterations/smoke.toml",
        "--report-out",
        "iteration-report.json",
        "--markdown-out",
        "iteration.md",
        "--format",
        "json",
    ]);

    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let super::MergeCaptainCommand::Iterate(iterate) = args.command else {
        panic!("expected merge-captain iterate command");
    };
    assert_eq!(
        iterate.manifest.as_deref(),
        Some("examples/personas/merge_captain/iterations/smoke.toml")
    );
    assert_eq!(iterate.report_out.as_deref(), Some("iteration-report.json"));
    assert_eq!(iterate.markdown_out.as_deref(), Some("iteration.md"));
    assert!(!iterate.json);
    assert!(matches!(
        iterate.format,
        crate::cli::MergeCaptainIterateFormat::Json
    ));
}

#[test]
fn test_parses_merge_captain_iterate_diff_args() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "iterate",
        "--diff",
        "baseline",
        "candidate",
    ]);

    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let super::MergeCaptainCommand::Iterate(iterate) = args.command else {
        panic!("expected merge-captain iterate command");
    };
    assert_eq!(iterate.diff, vec!["baseline", "candidate"]);
    assert!(iterate.manifest.is_none());
}

#[test]
fn test_parses_trigger_replay_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trigger",
        "replay",
        "trigger_evt_123",
        "--diff",
        "--as-of",
        "2026-04-19T18:00:00Z",
    ]);

    let Command::Trigger(args) = cli.command.unwrap() else {
        panic!("expected trigger command");
    };
    let TriggerCommand::Replay(replay) = args.command else {
        panic!("expected trigger replay");
    };
    assert_eq!(replay.event_id.as_deref(), Some("trigger_evt_123"));
    assert!(replay.diff);
    assert_eq!(replay.as_of.as_deref(), Some("2026-04-19T18:00:00Z"));
    assert!(replay.where_expr.is_none());
}

#[test]
fn test_parses_trigger_replay_steering_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trigger",
        "replay",
        "trigger_evt_123",
        "--steer-from",
        "outcome",
        "--to-decision",
        r#"{"status":"skipped"}"#,
        "--reason",
        "human corrected routing",
        "--applied-by",
        "alice",
        "--scope",
        "this_persona",
    ]);

    let Command::Trigger(args) = cli.command.unwrap() else {
        panic!("expected trigger command");
    };
    let TriggerCommand::Replay(replay) = args.command else {
        panic!("expected trigger replay");
    };
    assert_eq!(replay.event_id.as_deref(), Some("trigger_evt_123"));
    assert_eq!(replay.steer_from.as_deref(), Some("outcome"));
    assert_eq!(
        replay.to_decision.as_deref(),
        Some(r#"{"status":"skipped"}"#)
    );
    assert_eq!(replay.reason.as_deref(), Some("human corrected routing"));
    assert_eq!(replay.applied_by.as_deref(), Some("alice"));
    assert_eq!(replay.scope.as_deref(), Some("this_persona"));
}

#[test]
fn test_parses_flow_replay_audit_flags() {
    let cli = Cli::parse_from([
        "harn",
        "flow",
        "replay-audit",
        "--store",
        ".harn/flow.sqlite",
        "--predicate-root",
        ".",
        "--touched-dir",
        "crates/harn-vm",
        "--since",
        "2026-04-26",
        "--fail-on-drift",
        "--json",
    ]);

    let Command::Flow(args) = cli.command.unwrap() else {
        panic!("expected flow command");
    };
    let FlowCommand::ReplayAudit(audit) = args.command else {
        panic!("expected replay-audit command");
    };
    assert_eq!(audit.store, PathBuf::from(".harn/flow.sqlite"));
    assert_eq!(audit.predicate_root, PathBuf::from("."));
    assert_eq!(audit.touched_dirs, vec![PathBuf::from("crates/harn-vm")]);
    assert_eq!(audit.since.as_deref(), Some("2026-04-26"));
    assert!(audit.fail_on_drift);
    assert!(audit.json);
}

#[test]
fn test_parses_flow_archivist_scan_flags() {
    let cli = Cli::parse_from([
        "harn",
        "flow",
        "archivist",
        "scan",
        ".",
        "--manifest",
        "examples/personas/flow.harn.toml",
        "--store",
        ".harn/flow.sqlite",
        "--shadow-days",
        "14",
        "--out",
        ".harn/archivist/proposals.json",
        "--json",
    ]);

    let Command::Flow(args) = cli.command.unwrap() else {
        panic!("expected flow command");
    };
    let FlowCommand::Archivist(archivist) = args.command else {
        panic!("expected archivist command");
    };
    let FlowArchivistCommand::Scan(scan) = archivist.command;
    assert_eq!(scan.repo, PathBuf::from("."));
    assert_eq!(
        scan.manifest,
        Some(PathBuf::from("examples/personas/flow.harn.toml"))
    );
    assert_eq!(scan.store, PathBuf::from(".harn/flow.sqlite"));
    assert_eq!(scan.shadow_days, 14);
    assert_eq!(
        scan.out,
        Some(PathBuf::from(".harn/archivist/proposals.json"))
    );
    assert!(scan.json);
}

#[test]
fn test_parses_persona_check_flags() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "--manifest",
        "examples/personas/harn.toml",
        "check",
        "personas/ship_captain/harn.toml",
        "--json",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    assert_eq!(
        args.manifest,
        Some(PathBuf::from("examples/personas/harn.toml"))
    );
    let PersonaCommand::Check(check) = args.command else {
        panic!("expected persona check command");
    };
    assert_eq!(
        check.path,
        Some(PathBuf::from("personas/ship_captain/harn.toml"))
    );
    assert!(check.json);
}

#[test]
fn test_parses_test_determinism_flag() {
    let cli = Cli::parse_from([
        "harn",
        "test",
        "--determinism",
        "--filter",
        "agent",
        "tests/agent_loop.harn",
    ]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert!(args.determinism);
    assert_eq!(args.filter.as_deref(), Some("agent"));
    assert_eq!(args.target.as_deref(), Some("tests/agent_loop.harn"));
}

#[test]
fn test_parses_trigger_bulk_cancel_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trigger",
        "cancel",
        "--where",
        "event.payload.tenant == 'acme' AND attempt.handler == 'handlers::risky'",
        "--dry-run",
        "--progress",
        "--rate-limit",
        "4",
    ]);

    let Command::Trigger(args) = cli.command.unwrap() else {
        panic!("expected trigger command");
    };
    let TriggerCommand::Cancel(cancel) = args.command else {
        panic!("expected trigger cancel");
    };
    assert!(cancel.event_id.is_none());
    assert_eq!(
        cancel.where_expr.as_deref(),
        Some("event.payload.tenant == 'acme' AND attempt.handler == 'handlers::risky'")
    );
    assert!(cancel.dry_run);
    assert!(cancel.progress);
    assert_eq!(cancel.rate_limit, Some(4.0));
}

#[test]
fn test_parses_trust_query_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trust",
        "query",
        "--agent",
        "github-triage-bot",
        "--action",
        "github.issue.opened",
        "--since",
        "2026-04-19T18:00:00Z",
        "--until",
        "2026-04-19T19:00:00Z",
        "--tier",
        "act-auto",
        "--outcome",
        "success",
        "--limit",
        "500",
        "--grouped-by-trace",
        "--json",
        "--summary",
    ]);

    let Command::Trust(args) = cli.command.unwrap() else {
        panic!("expected trust command");
    };
    let TrustCommand::Query(query) = args.command else {
        panic!("expected trust query");
    };
    assert_eq!(query.agent.as_deref(), Some("github-triage-bot"));
    assert_eq!(query.action.as_deref(), Some("github.issue.opened"));
    assert_eq!(query.since.as_deref(), Some("2026-04-19T18:00:00Z"));
    assert_eq!(query.until.as_deref(), Some("2026-04-19T19:00:00Z"));
    assert!(matches!(query.tier, Some(TrustTierArg::ActAuto)));
    assert!(matches!(query.outcome, Some(TrustOutcomeArg::Success)));
    assert_eq!(query.limit, Some(500));
    assert!(query.grouped_by_trace);
    assert!(query.json);
    assert!(query.summary);
}

#[test]
fn test_parses_trust_demote_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trust",
        "demote",
        "github-triage-bot",
        "--to",
        "shadow",
        "--reason",
        "unexpected mutation",
    ]);

    let Command::Trust(args) = cli.command.unwrap() else {
        panic!("expected trust command");
    };
    let TrustCommand::Demote(demote) = args.command else {
        panic!("expected trust demote");
    };
    assert_eq!(demote.agent, "github-triage-bot");
    assert!(matches!(demote.to, TrustTierArg::Shadow));
    assert_eq!(demote.reason, "unexpected mutation");
}

#[test]
fn test_parses_trust_graph_verify_chain() {
    let cli = Cli::parse_from(["harn", "trust-graph", "verify-chain", "--json"]);

    let Command::TrustGraph(args) = cli.command.unwrap() else {
        panic!("expected trust-graph command");
    };
    let TrustCommand::VerifyChain(verify) = args.command else {
        panic!("expected trust-graph verify-chain");
    };
    assert!(verify.json);
}

#[test]
fn test_parses_portal_flags() {
    let cli = Cli::parse_from([
        "harn", "portal", "--dir", "runs", "--host", "0.0.0.0", "--port", "4900", "--open", "false",
    ]);

    let Command::Portal(args) = cli.command.unwrap() else {
        panic!("expected portal command");
    };
    assert_eq!(args.dir, "runs");
    assert_eq!(args.host, "0.0.0.0");
    assert_eq!(args.port, 4900);
    assert!(!args.open);
}

#[test]
fn test_parses_orchestrator_serve_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "serve",
        "--config",
        "workspace/harn.toml",
        "--state-dir",
        "state/orchestrator",
        "--bind",
        "0.0.0.0:8080",
        "--cert",
        "tls/cert.pem",
        "--key",
        "tls/key.pem",
        "--shutdown-timeout",
        "45",
        "--drain-max-items",
        "256",
        "--drain-deadline",
        "9",
        "--pump-max-outstanding",
        "4",
        "--mcp",
        "--mcp-path",
        "/ops/mcp",
        "--mcp-sse-path",
        "/ops/sse",
        "--mcp-messages-path",
        "/ops/messages",
        "--log-format",
        "json",
        "--role",
        "single-tenant",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Serve(serve) = args.command else {
        panic!("expected orchestrator serve");
    };
    assert_eq!(serve.local.config, PathBuf::from("workspace/harn.toml"));
    assert_eq!(serve.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(serve.bind.to_string(), "0.0.0.0:8080");
    assert_eq!(serve.cert, Some(PathBuf::from("tls/cert.pem")));
    assert_eq!(serve.key, Some(PathBuf::from("tls/key.pem")));
    assert_eq!(serve.shutdown_timeout, 45);
    assert_eq!(serve.drain_max_items, Some(256));
    assert_eq!(serve.drain_deadline, Some(9));
    assert_eq!(serve.pump_max_outstanding, Some(4));
    assert!(serve.mcp);
    assert_eq!(serve.mcp_path, "/ops/mcp");
    assert_eq!(serve.mcp_sse_path, "/ops/sse");
    assert_eq!(serve.mcp_messages_path, "/ops/messages");
    assert_eq!(serve.log_format, OrchestratorLogFormat::Json);
    assert_eq!(
        serve.role,
        crate::commands::orchestrator::role::OrchestratorRole::SingleTenant
    );
}

#[test]
fn test_parses_orchestrator_serve_container_aliases() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "serve",
        "--manifest",
        "/etc/harn/triggers.toml",
        "--state-dir",
        "/var/lib/harn/state",
        "--listen",
        "0.0.0.0:8080",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Serve(serve) = args.command else {
        panic!("expected orchestrator serve");
    };
    assert_eq!(serve.local.config, PathBuf::from("/etc/harn/triggers.toml"));
    assert_eq!(serve.local.state_dir, PathBuf::from("/var/lib/harn/state"));
    assert_eq!(serve.bind.to_string(), "0.0.0.0:8080");
}

#[test]
fn test_parses_orchestrator_deploy_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "deploy",
        "--provider",
        "fly",
        "--manifest",
        "workspace/harn.toml",
        "--name",
        "harn-prod",
        "--image",
        "ghcr.io/acme/harn-prod:latest",
        "--deploy-dir",
        "ops/deploy",
        "--port",
        "8443",
        "--data-dir",
        "/data",
        "--disk-size-gb",
        "20",
        "--shutdown-timeout",
        "60",
        "--region",
        "sjc",
        "--fly-api-token",
        "fly-token",
        "--build",
        "--env",
        "RUST_LOG=debug",
        "--secret",
        "OPENAI_API_KEY=sk-test",
        "--dry-run",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Deploy(deploy) = args.command else {
        panic!("expected orchestrator deploy");
    };
    assert_eq!(deploy.provider, OrchestratorDeployProvider::Fly);
    assert_eq!(deploy.manifest, PathBuf::from("workspace/harn.toml"));
    assert_eq!(deploy.name, "harn-prod");
    assert_eq!(deploy.image, "ghcr.io/acme/harn-prod:latest");
    assert_eq!(deploy.deploy_dir, PathBuf::from("ops/deploy"));
    assert_eq!(deploy.port, 8443);
    assert_eq!(deploy.data_dir, "/data");
    assert_eq!(deploy.disk_size_gb, 20);
    assert_eq!(deploy.shutdown_timeout, 60);
    assert_eq!(deploy.region.as_deref(), Some("sjc"));
    assert_eq!(deploy.fly_api_token.as_deref(), Some("fly-token"));
    assert!(deploy.build);
    assert_eq!(deploy.env, vec!["RUST_LOG=debug".to_string()]);
    assert_eq!(deploy.secret, vec!["OPENAI_API_KEY=sk-test".to_string()]);
    assert!(deploy.dry_run);
}

#[test]
fn test_parses_orchestrator_inspect_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "inspect",
        "--config",
        "workspace/harn.toml",
        "--state-dir",
        "state/orchestrator",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Inspect(inspect) = args.command else {
        panic!("expected orchestrator inspect");
    };
    assert_eq!(inspect.local.config, PathBuf::from("workspace/harn.toml"));
    assert_eq!(inspect.local.state_dir, PathBuf::from("state/orchestrator"));
    assert!(!inspect.json);
}

#[test]
fn test_parses_orchestrator_fire_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "fire",
        "github-new-issue",
        "--config",
        "workspace/harn.toml",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Fire(fire) = args.command else {
        panic!("expected orchestrator fire");
    };
    assert_eq!(fire.binding_id, "github-new-issue");
    assert_eq!(fire.local.config, PathBuf::from("workspace/harn.toml"));
}

#[test]
fn test_parses_orchestrator_replay_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "replay",
        "trigger_evt_123",
        "--state-dir",
        "state/orchestrator",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Replay(replay) = args.command else {
        panic!("expected orchestrator replay");
    };
    assert_eq!(replay.event_id, "trigger_evt_123");
    assert_eq!(replay.local.state_dir, PathBuf::from("state/orchestrator"));
    assert!(!replay.json);
}

#[test]
fn test_parses_orchestrator_dlq_replay_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "dlq",
        "--replay",
        "dlq_123",
        "--config",
        "workspace/harn.toml",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Dlq(dlq) = args.command else {
        panic!("expected orchestrator dlq");
    };
    assert_eq!(dlq.replay.as_deref(), Some("dlq_123"));
    assert!(dlq.discard.is_none());
    assert!(!dlq.list);
    assert_eq!(dlq.local.config, PathBuf::from("workspace/harn.toml"));
    assert!(!dlq.json);
}

#[test]
fn test_parses_orchestrator_json_flags() {
    let inspect_cli = Cli::parse_from(["harn", "orchestrator", "inspect", "--json"]);
    let Command::Orchestrator(inspect_args) = inspect_cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Inspect(inspect) = inspect_args.command else {
        panic!("expected orchestrator inspect");
    };
    assert!(inspect.json);

    let replay_cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "replay",
        "trigger_evt_123",
        "--json",
    ]);
    let Command::Orchestrator(replay_args) = replay_cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Replay(replay) = replay_args.command else {
        panic!("expected orchestrator replay");
    };
    assert!(replay.json);

    let dlq_cli = Cli::parse_from(["harn", "orchestrator", "dlq", "--json", "--list"]);
    let Command::Orchestrator(dlq_args) = dlq_cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Dlq(dlq) = dlq_args.command else {
        panic!("expected orchestrator dlq");
    };
    assert!(dlq.json);
    assert!(dlq.list);
}

#[test]
fn test_parses_orchestrator_resume_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "resume",
        "hitl_escalation_trigger_evt_123_1",
        "--reviewer",
        "ops-lead",
        "--reason",
        "manual escalation ack",
        "--json",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Resume(resume) = args.command else {
        panic!("expected orchestrator resume");
    };
    assert_eq!(resume.event_id, "hitl_escalation_trigger_evt_123_1");
    assert_eq!(resume.reviewer, "ops-lead");
    assert_eq!(resume.reason.as_deref(), Some("manual escalation ack"));
    assert!(resume.json);
}

#[test]
fn test_parses_orchestrator_queue_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "queue",
        "--state-dir",
        "state/orchestrator",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Queue(queue) = args.command else {
        panic!("expected orchestrator queue");
    };
    assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
    assert!(queue.command.is_none());
}

#[test]
fn test_parses_orchestrator_queue_drain_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "queue",
        "--state-dir",
        "state/orchestrator",
        "drain",
        "triage",
        "--consumer-id",
        "worker-a",
        "--claim-ttl",
        "30s",
        "--json",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Queue(queue) = args.command else {
        panic!("expected orchestrator queue");
    };
    let Some(OrchestratorQueueCommand::Drain(drain)) = queue.command else {
        panic!("expected orchestrator queue drain");
    };
    assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(drain.queue, "triage");
    assert_eq!(drain.consumer_id.as_deref(), Some("worker-a"));
    assert_eq!(drain.claim_ttl, StdDuration::from_secs(30));
    assert!(drain.json);
}

#[test]
fn test_parses_orchestrator_queue_purge_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "queue",
        "--state-dir",
        "state/orchestrator",
        "purge",
        "triage",
        "--confirm",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Queue(queue) = args.command else {
        panic!("expected orchestrator queue");
    };
    let Some(OrchestratorQueueCommand::Purge(purge)) = queue.command else {
        panic!("expected orchestrator queue purge");
    };
    assert_eq!(queue.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(purge.queue, "triage");
    assert!(purge.confirm);
}

#[test]
fn test_parses_orchestrator_recover_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "recover",
        "--config",
        "workspace/harn.toml",
        "--state-dir",
        "state/orchestrator",
        "--envelope-age",
        "15m",
        "--dry-run",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Recover(recover) = args.command else {
        panic!("expected orchestrator recover");
    };
    assert_eq!(recover.local.config, PathBuf::from("workspace/harn.toml"));
    assert_eq!(recover.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(recover.envelope_age, StdDuration::from_mins(15));
    assert!(recover.dry_run);
    assert!(!recover.yes);
}

#[test]
fn test_parses_orchestrator_tenant_create_args() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "tenant",
        "--state-dir",
        "state/orchestrator",
        "create",
        "acme",
        "--daily-cost-usd",
        "25.5",
        "--ingest-per-minute",
        "120",
        "--json",
    ]);

    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Tenant(tenant) = args.command else {
        panic!("expected orchestrator tenant");
    };
    let OrchestratorTenantCommand::Create(create) = tenant.command else {
        panic!("expected orchestrator tenant create");
    };
    assert_eq!(tenant.local.state_dir, PathBuf::from("state/orchestrator"));
    assert_eq!(create.id, "acme");
    assert_eq!(create.daily_cost_usd, Some(25.5));
    assert_eq!(create.ingest_per_minute, Some(120));
    assert!(create.json);
}

#[test]
fn test_parses_new_template() {
    let cli = Cli::parse_from(["harn", "new", "review-bot", "--template", "agent"]);

    let Command::New(args) = cli.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(args.first.as_deref(), Some("review-bot"));
    assert_eq!(args.second.as_deref(), None);
    assert_eq!(args.template, Some(ProjectTemplate::Agent));
}

#[test]
fn test_parses_new_package_kind() {
    let cli = Cli::parse_from(["harn", "new", "package", "acme-lib"]);

    let Command::New(args) = cli.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(args.first.as_deref(), Some("package"));
    assert_eq!(args.second.as_deref(), Some("acme-lib"));
    assert_eq!(args.template, None);
}

#[test]
fn test_parses_pack_signing_flags() {
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "examples/hello.harn",
        "--sign",
        "--key",
        "release.pem",
        "--out",
        "hello.harnpack",
    ]);

    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    assert!(args.command.is_none(), "build path takes no subcommand");
    assert_eq!(args.entrypoint, Some(PathBuf::from("examples/hello.harn")));
    assert_eq!(args.key, Some(PathBuf::from("release.pem")));
    assert_eq!(args.out, Some(PathBuf::from("hello.harnpack")));
    assert!(args.sign);
    assert!(!args.unsigned);
}

#[test]
fn test_parses_pack_exclude_secrets_flag() {
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "examples/hello.harn",
        "--unsigned",
        "--exclude-secrets",
    ]);
    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    assert!(args.exclude_secrets);
    assert!(!args.include_secrets);
}

#[test]
fn test_parses_pack_verify_subcommand() {
    use crate::cli::PackCommand;
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "verify",
        "bundle.harnpack",
        "--allow-unsigned",
        "--trust-policy",
        "policy.json",
        "--require-trusted-signer",
        "--strict",
        "--json",
    ]);
    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    let Some(PackCommand::Verify(verify)) = args.command else {
        panic!("expected pack verify subcommand");
    };
    assert_eq!(verify.bundle, PathBuf::from("bundle.harnpack"));
    assert!(verify.allow_unsigned);
    assert_eq!(verify.trust_policy, Some(PathBuf::from("policy.json")));
    assert!(verify.require_trusted_signer);
    assert!(verify.strict);
    assert!(verify.json);
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
fn test_parses_doctor_flags() {
    let cli = Cli::parse_from([
        "harn",
        "doctor",
        "--json",
        "--check-providers",
        "--check-targets",
    ]);

    let Command::Doctor(args) = cli.command.unwrap() else {
        panic!("expected doctor command");
    };
    assert!(args.json);
    assert!(args.check_providers);
    assert!(args.check_targets);
}

#[test]
fn test_parses_install_integrity_flags() {
    let cli = Cli::parse_from(["harn", "install", "--locked", "--offline"]);

    let Command::Install(args) = cli.command.unwrap() else {
        panic!("expected install command");
    };
    assert!(!args.frozen);
    assert!(args.locked);
    assert!(args.offline);
}

#[test]
fn test_parses_add_registry_override() {
    let cli = Cli::parse_from([
        "harn",
        "add",
        "@burin/notion-sdk@1.2.3",
        "--registry",
        "index.toml",
    ]);

    let Command::Add(args) = cli.command.unwrap() else {
        panic!("expected add command");
    };
    assert_eq!(args.name_or_spec, "@burin/notion-sdk@1.2.3");
    assert_eq!(args.registry.as_deref(), Some("index.toml"));
}

#[test]
fn test_parses_package_cache_subcommands() {
    let cli = Cli::parse_from(["harn", "package", "list", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::List(list) = args.command else {
        panic!("expected package list");
    };
    assert!(list.json);

    let cli = Cli::parse_from(["harn", "package", "doctor", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Doctor(doctor) = args.command else {
        panic!("expected package doctor");
    };
    assert!(doctor.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "search",
        "notion",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Search(search) = args.command else {
        panic!("expected package search");
    };
    assert_eq!(search.query.as_deref(), Some("notion"));
    assert_eq!(search.registry.as_deref(), Some("index.toml"));
    assert!(search.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "info",
        "@burin/notion-sdk@1.2.3",
        "--registry",
        "index.toml",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Info(info) = args.command else {
        panic!("expected package info");
    };
    assert_eq!(info.name, "@burin/notion-sdk@1.2.3");
    assert_eq!(info.registry.as_deref(), Some("index.toml"));

    let cli = Cli::parse_from(["harn", "package", "check", "pkg", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Check(check) = args.command else {
        panic!("expected package check");
    };
    assert_eq!(check.package, Some(PathBuf::from("pkg")));
    assert!(check.json);

    let cli = Cli::parse_from(["harn", "package", "pack", "pkg", "--dry-run"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Pack(pack) = args.command else {
        panic!("expected package pack");
    };
    assert_eq!(pack.package, Some(PathBuf::from("pkg")));
    assert!(pack.dry_run);

    let cli = Cli::parse_from(["harn", "package", "docs", "pkg", "--check"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Docs(docs) = args.command else {
        panic!("expected package docs");
    };
    assert_eq!(docs.package, Some(PathBuf::from("pkg")));
    assert!(docs.check);

    let cli = Cli::parse_from(["harn", "package", "cache", "list"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    assert!(matches!(cache.command, PackageCacheCommand::List));

    let cli = Cli::parse_from(["harn", "package", "cache", "clean", "--all"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    let PackageCacheCommand::Clean(clean) = cache.command else {
        panic!("expected package cache clean");
    };
    assert!(clean.all);

    let cli = Cli::parse_from(["harn", "package", "cache", "verify", "--materialized"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    let PackageCacheCommand::Verify(verify) = cache.command else {
        panic!("expected package cache verify");
    };
    assert!(verify.materialized);
}

#[test]
fn test_parses_publish_git_tag_flow_options() {
    let cli = Cli::parse_from([
        "harn",
        "publish",
        "pkg",
        "--dry-run",
        "--remote",
        "upstream",
        "--index-repo",
        "burin-labs/harn-cloud",
        "--index-path",
        "package-index/harn-package-index.toml",
        "--registry-name",
        "@burin/acme-lib",
        "--skip-index-pr",
        "--json",
    ]);
    let Command::Publish(PublishArgs {
        package,
        dry_run,
        remote,
        index_repo,
        index_path,
        registry_name,
        skip_index_pr,
        json,
        ..
    }) = cli.command.unwrap()
    else {
        panic!("expected publish command");
    };

    assert_eq!(package, Some(PathBuf::from("pkg")));
    assert!(dry_run);
    assert_eq!(remote, "upstream");
    assert_eq!(index_repo, "burin-labs/harn-cloud");
    assert_eq!(
        index_path,
        PathBuf::from("package-index/harn-package-index.toml")
    );
    assert_eq!(registry_name.as_deref(), Some("@burin/acme-lib"));
    assert!(skip_index_pr);
    assert!(json);
}

#[test]
fn test_parses_rule_publish_and_search() {
    let cli = Cli::parse_from([
        "harn",
        "rule",
        "publish",
        "pkg",
        "--dry-run",
        "--registry-name",
        "@acme/rules",
        "--skip-index-pr",
        "--json",
    ]);
    let Command::Rule(args) = cli.command.unwrap() else {
        panic!("expected rule command");
    };
    let RuleCommand::Publish(publish) = args.command else {
        panic!("expected rule publish");
    };
    assert_eq!(publish.package, Some(PathBuf::from("pkg")));
    assert!(publish.dry_run);
    assert_eq!(publish.registry_name.as_deref(), Some("@acme/rules"));
    assert!(publish.skip_index_pr);
    assert!(publish.json);

    let cli = Cli::parse_from([
        "harn",
        "rule",
        "search",
        "typescript",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Rule(args) = cli.command.unwrap() else {
        panic!("expected rule command");
    };
    let RuleCommand::Search(search) = args.command else {
        panic!("expected rule search");
    };
    assert_eq!(search.query.as_deref(), Some("typescript"));
    assert_eq!(search.registry.as_deref(), Some("index.toml"));
    assert!(search.json);
}

#[test]
fn test_parses_package_scaffold_openapi() {
    let cli = Cli::parse_from([
        "harn",
        "package",
        "scaffold",
        "openapi",
        "--name",
        "acme-sdk-harn",
        "--module-name",
        "acme_sdk",
        "--client-name",
        "AcmeClient",
        "--spec",
        "./openapi.json",
        "--out",
        "./acme-sdk-harn",
        "--default-base-url",
        "https://api.example.test",
        "--harn-openapi-path",
        "../harn-openapi",
        "--harn-openapi-git",
        "https://github.com/burin-labs/harn-openapi",
        "--harn-openapi-rev",
        "abc123",
        "--force",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Scaffold(scaffold) = args.command else {
        panic!("expected package scaffold");
    };
    let PackageScaffoldCommand::Openapi(openapi) = scaffold.command;
    assert_eq!(openapi.name, "acme-sdk-harn");
    assert_eq!(openapi.module_name.as_deref(), Some("acme_sdk"));
    assert_eq!(openapi.client_name.as_deref(), Some("AcmeClient"));
    assert_eq!(openapi.spec, "./openapi.json");
    assert_eq!(openapi.out, Some(PathBuf::from("./acme-sdk-harn")));
    assert_eq!(
        openapi.default_base_url.as_deref(),
        Some("https://api.example.test")
    );
    assert_eq!(
        openapi.harn_openapi_path,
        Some(PathBuf::from("../harn-openapi"))
    );
    assert_eq!(
        openapi.harn_openapi_git.as_deref(),
        Some("https://github.com/burin-labs/harn-openapi")
    );
    assert_eq!(openapi.harn_openapi_rev.as_deref(), Some("abc123"));
    assert!(openapi.force);
}

#[test]
fn test_parses_tool_new_and_skill_new_alias() {
    let cli = Cli::parse_from([
        "harn",
        "tool",
        "new",
        "acme-tool",
        "--description",
        "Echo text",
        "--dir",
        "packages/acme-tool",
        "--force",
    ]);
    let Command::Tool(args) = cli.command.unwrap() else {
        panic!("expected tool command");
    };
    let ToolCommand::New(new) = args.command;
    assert_eq!(new.name, "acme-tool");
    assert_eq!(new.description.as_deref(), Some("Echo text"));
    assert_eq!(new.dir.as_deref(), Some("packages/acme-tool"));
    assert!(new.force);

    let cli = Cli::parse_from(["harn", "skill", "new", "deploy", "--description", "Deploy"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::New(new) = args.command else {
        panic!("expected skill new alias");
    };
    assert_eq!(new.name, "deploy");
    assert_eq!(new.description.as_deref(), Some("Deploy"));
}

#[test]
fn test_parses_package_outdated_audit_artifacts() {
    let cli = Cli::parse_from([
        "harn",
        "package",
        "outdated",
        "--remote",
        "--refresh",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Outdated(outdated) = args.command else {
        panic!("expected package outdated");
    };
    assert!(outdated.remote);
    assert!(outdated.refresh);
    assert_eq!(outdated.registry.as_deref(), Some("index.toml"));
    assert!(outdated.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "audit",
        "--registry",
        "index.toml",
        "--skip-materialized",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Audit(audit) = args.command else {
        panic!("expected package audit");
    };
    assert_eq!(audit.registry.as_deref(), Some("index.toml"));
    assert!(audit.skip_materialized);
    assert!(audit.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "artifacts",
        "manifest",
        "--output",
        "vendor/manifest.json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Artifacts(artifacts) = args.command else {
        panic!("expected package artifacts");
    };
    let PackageArtifactsCommand::Manifest(manifest) = artifacts.command else {
        panic!("expected artifacts manifest");
    };
    assert_eq!(manifest.output, Some(PathBuf::from("vendor/manifest.json")));

    let cli = Cli::parse_from([
        "harn",
        "package",
        "artifacts",
        "check",
        "vendor/manifest.json",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Artifacts(artifacts) = args.command else {
        panic!("expected package artifacts");
    };
    let PackageArtifactsCommand::Check(check) = artifacts.command else {
        panic!("expected artifacts check");
    };
    assert_eq!(check.manifest, PathBuf::from("vendor/manifest.json"));
    assert!(check.json);
}

#[test]
fn test_install_and_update_accept_json_flag() {
    let cli = Cli::parse_from(["harn", "install", "--frozen", "--json"]);
    let Command::Install(install) = cli.command.unwrap() else {
        panic!("expected install command");
    };
    assert!(install.frozen);
    assert!(install.json);

    let cli = Cli::parse_from(["harn", "update", "--all", "--json"]);
    let Command::Update(update) = cli.command.unwrap() else {
        panic!("expected update command");
    };
    assert!(update.all);
    assert!(update.json);
}

#[test]
fn test_parses_connector_test_args() {
    let cli = Cli::parse_from([
        "harn",
        "connector",
        "test",
        "pkg",
        "--provider",
        "notion",
        "--run-poll-tick",
        "--json",
    ]);
    let Command::Connector(args) = cli.command.unwrap() else {
        panic!("expected connector command");
    };
    let ConnectorCommand::Test(test) = args.command else {
        panic!("expected connector test");
    };
    assert_eq!(test.package, "pkg");
    assert_eq!(test.providers, vec!["notion"]);
    assert!(test.run_poll_tick);
    assert!(test.json);
}

#[test]
fn test_parses_viz_args() {
    let cli = Cli::parse_from(["harn", "viz", "main.harn", "--output", "graph.mmd"]);

    let Command::Viz(args) = cli.command.unwrap() else {
        panic!("expected viz command");
    };
    assert_eq!(args.file, "main.harn");
    assert_eq!(args.output.as_deref(), Some("graph.mmd"));
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
fn test_parses_skills_subcommands() {
    let cli = Cli::parse_from(["harn", "skills", "list", "--json"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::List(list) = args.command else {
        panic!("expected skills list");
    };
    assert!(list.json);

    let cli = Cli::parse_from(["harn", "skills", "get", "harn-language", "--full", "--json"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Get(get) = args.command else {
        panic!("expected skills get");
    };
    assert_eq!(get.name, "harn-language");
    assert!(get.full);
    assert!(get.json);

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "dump",
        "--all",
        "--out",
        "/tmp/skills",
        "--force",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Dump(dump) = args.command else {
        panic!("expected skills dump");
    };
    assert!(dump.all);
    assert_eq!(dump.out.as_deref(), Some("/tmp/skills"));
    assert!(dump.force);

    let cli = Cli::parse_from(["harn", "skills", "resolved", "--json", "--all"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Resolved(resolved) = args.command else {
        panic!("expected skills resolved");
    };
    assert!(resolved.json);
    assert!(resolved.all);

    let cli = Cli::parse_from(["harn", "skills", "match", "deploy the app", "--top-n", "3"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Match(matcher) = args.command else {
        panic!("expected skills match");
    };
    assert_eq!(matcher.query, "deploy the app");
    assert_eq!(matcher.top_n, 3);

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "install",
        "https://example.com/acme/harn-skills.git",
        "--tag",
        "v1.0",
        "--namespace",
        "acme",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Install(install) = args.command else {
        panic!("expected skills install");
    };
    assert_eq!(install.tag.as_deref(), Some("v1.0"));
    assert_eq!(install.namespace.as_deref(), Some("acme"));

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "new",
        "deploy",
        "--description",
        "Ship things",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::New(new_args) = args.command else {
        panic!("expected skills new");
    };
    assert_eq!(new_args.name, "deploy");
    assert_eq!(new_args.description.as_deref(), Some("Ship things"));
}

#[test]
fn test_parses_skill_provenance_subcommands() {
    let cli = Cli::parse_from(["harn", "skill", "key", "generate", "--out", "signer.pem"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Key(key_args) = args.command else {
        panic!("expected skill key");
    };
    let SkillKeyCommand::Generate(generate) = key_args.command;
    assert_eq!(generate.out, "signer.pem");

    let cli = Cli::parse_from(["harn", "skill", "sign", "SKILL.md", "--key", "signer.pem"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Sign(sign) = args.command else {
        panic!("expected skill sign");
    };
    assert_eq!(sign.skill, "SKILL.md");
    assert_eq!(sign.key, "signer.pem");

    let cli = Cli::parse_from([
        "harn",
        "skill",
        "endorse",
        "SKILL.md",
        "--key",
        "auditor.pem",
    ]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Endorse(endorse) = args.command else {
        panic!("expected skill endorse");
    };
    assert_eq!(endorse.skill, "SKILL.md");
    assert_eq!(endorse.key, "auditor.pem");

    let cli = Cli::parse_from(["harn", "skill", "verify", "SKILL.md", "--json"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Verify(verify) = args.command else {
        panic!("expected skill verify");
    };
    assert_eq!(verify.skill, "SKILL.md");
    assert!(verify.json);

    let cli = Cli::parse_from(["harn", "skill", "who-signed", "SKILL.md", "--json"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::WhoSigned(who_signed) = args.command else {
        panic!("expected skill who-signed");
    };
    assert_eq!(who_signed.skill, "SKILL.md");
    assert!(who_signed.json);

    let cli = Cli::parse_from([
        "harn",
        "skill",
        "trust",
        "add",
        "--from",
        "https://example.com/signer.pub",
    ]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Trust(trust) = args.command else {
        panic!("expected skill trust");
    };
    let SkillTrustCommand::Add(add) = trust.command else {
        panic!("expected skill trust add");
    };
    assert_eq!(add.from, "https://example.com/signer.pub");

    let cli = Cli::parse_from(["harn", "skill", "trust", "list"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Trust(trust) = args.command else {
        panic!("expected skill trust");
    };
    assert!(matches!(trust.command, SkillTrustCommand::List(_)));
}

#[test]
fn test_parses_model_info_args() {
    let cli = Cli::parse_from([
        "harn",
        "model-info",
        "--verify",
        "--warm",
        "--keep-alive",
        "forever",
        "tog-gemma4-31b",
    ]);

    let Command::ModelInfo(args) = cli.command.unwrap() else {
        panic!("expected model-info command");
    };
    assert_eq!(args.model, "tog-gemma4-31b");
    assert!(args.verify);
    assert!(args.warm);
    assert_eq!(args.keep_alive.as_deref(), Some("forever"));
}

#[test]
fn test_parses_models_test_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "test",
        "qwen3:30b",
        "--provider",
        "ollama",
        "--prompt",
        "say pong",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Test(args) = args.command else {
        panic!("expected models test command");
    };
    assert_eq!(args.model, "qwen3:30b");
    assert_eq!(args.provider.as_deref(), Some("ollama"));
    assert_eq!(args.prompt, "say pong");
    assert!(args.json);
}

#[test]
fn test_parses_local_list_args() {
    let cli = Cli::parse_from(["harn", "local", "list", "--json", "--provider", "ollama"]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::List(args) = args.command else {
        panic!("expected local list command");
    };
    assert!(args.json);
    assert_eq!(args.provider.as_deref(), Some("ollama"));
}

#[test]
fn test_parses_local_switch_args_with_machine_overrides() {
    let cli = Cli::parse_from([
        "harn",
        "local",
        "switch",
        "qwen36-coder",
        "--provider",
        "ollama",
        "--ctx",
        "65536",
        "--keep-alive",
        "1h",
        "--no-pull",
        "--no-evict",
        "--force",
        "--passed-probe",
        "two_turn_cache_probe",
        "--probe-result",
        "probe.json",
        "--json",
    ]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::Switch(args) = args.command else {
        panic!("expected local switch command");
    };
    assert_eq!(args.model, "qwen36-coder");
    assert_eq!(args.provider.as_deref(), Some("ollama"));
    assert_eq!(args.ctx, Some(65536));
    assert_eq!(args.keep_alive.as_deref(), Some("1h"));
    assert!(args.no_pull);
    assert!(args.no_evict);
    assert!(args.force);
    assert_eq!(args.passed_probes, vec!["two_turn_cache_probe".to_string()]);
    assert_eq!(args.probe_results, vec![PathBuf::from("probe.json")]);
    assert!(args.json);
}

#[test]
fn test_parses_local_profile_args() {
    let cli = Cli::parse_from([
        "harn",
        "local",
        "profile",
        "devstral-small-2",
        "--provider",
        "llamacpp",
        "--json",
    ]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::Profile(args) = args.command else {
        panic!("expected local profile command");
    };
    assert_eq!(args.model, "devstral-small-2");
    assert_eq!(args.provider.as_deref(), Some("llamacpp"));
    assert!(args.json);
}

#[test]
fn test_parses_local_stop_all_flag() {
    let cli = Cli::parse_from(["harn", "local", "stop", "--all", "--json"]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::Stop(args) = args.command else {
        panic!("expected local stop command");
    };
    assert!(args.all);
    assert!(args.json);
    assert!(args.provider.is_none());
}

#[test]
fn test_parses_providers_refresh_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "refresh",
        "--live",
        "--check",
        "--script",
        "scripts/update_provider_catalog.harn",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Refresh(args) = args.command else {
        panic!("expected providers refresh command");
    };
    assert!(args.live);
    assert!(args.check);
    assert!(!args.update);
    assert_eq!(
        args.script,
        std::path::PathBuf::from("scripts/update_provider_catalog.harn")
    );
}

#[test]
fn test_parses_providers_validate_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "validate",
        "--overlay",
        "providers.local.toml",
        "--check-artifacts",
        "--artifact-dir",
        "spec/provider-catalog",
        "--json",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Validate(args) = args.command else {
        panic!("expected providers validate command");
    };
    assert_eq!(
        args.overlay.as_deref(),
        Some(std::path::Path::new("providers.local.toml"))
    );
    assert!(args.check_artifacts);
    assert_eq!(
        args.artifact_dir,
        std::path::PathBuf::from("spec/provider-catalog")
    );
    assert!(args.json);
}

#[test]
fn test_parses_providers_export_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "export",
        "--output-dir",
        "tmp/catalog",
        "--check",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Export(args) = args.command else {
        panic!("expected providers export command");
    };
    assert_eq!(args.output_dir, std::path::PathBuf::from("tmp/catalog"));
    assert!(args.check);
}

#[test]
fn test_parses_providers_matrix_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "matrix",
        "--output",
        "tmp/provider-matrix.md",
        "--check",
        "--stdout",
        "--filter",
        "native_tools",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Matrix(args) = args.command else {
        panic!("expected providers matrix command");
    };
    assert_eq!(
        args.output,
        std::path::PathBuf::from("tmp/provider-matrix.md")
    );
    assert!(args.check);
    assert!(args.stdout);
    assert_eq!(args.filter.as_deref(), Some("native_tools"));
}

#[test]
fn test_parses_providers_support_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "support",
        "--output",
        "tmp/provider-support.md",
        "--json-output",
        "tmp/provider-support.json",
        "--notes",
        "provider_support_notes.toml",
        "--empirical",
        "summary.json",
        "--check",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Support(args) = args.command else {
        panic!("expected providers support command");
    };
    assert_eq!(
        args.output,
        std::path::PathBuf::from("tmp/provider-support.md")
    );
    assert_eq!(
        args.json_output,
        std::path::PathBuf::from("tmp/provider-support.json")
    );
    assert_eq!(
        args.notes,
        std::path::PathBuf::from("provider_support_notes.toml")
    );
    assert_eq!(
        args.empirical,
        vec![std::path::PathBuf::from("summary.json")]
    );
    assert!(args.check);
}

#[test]
fn test_parses_provider_catalog_args() {
    let cli = Cli::parse_from(["harn", "provider-catalog", "--available-only", "--refresh"]);

    let Command::ProviderCatalog(args) = cli.command.unwrap() else {
        panic!("expected provider-catalog command");
    };
    assert!(args.available_only);
    assert!(args.refresh);
}

#[test]
fn test_parses_provider_ready_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider-ready",
        "mlx",
        "--model",
        "mlx-qwen36-27b",
        "--base-url",
        "http://127.0.0.1:8002",
        "--json",
    ]);

    let Command::ProviderReady(args) = cli.command.unwrap() else {
        panic!("expected provider-ready command");
    };
    assert_eq!(args.provider, "mlx");
    assert_eq!(args.model.as_deref(), Some("mlx-qwen36-27b"));
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:8002"));
    assert!(args.json);
}

#[test]
fn test_parses_completions_args() {
    let cli = Cli::parse_from(["harn", "completions", "zsh"]);

    let Command::Completions(args) = cli.command.unwrap() else {
        panic!("expected completions command");
    };
    assert_eq!(args.shell, CompletionShell::Zsh);
}

#[test]
fn test_parses_provider_probe_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider-probe",
        "ollama",
        "--model",
        "devstral-small-2",
        "--base-url",
        "http://127.0.0.1:11434",
    ]);

    let Command::ProviderProbe(args) = cli.command.unwrap() else {
        panic!("expected provider-probe command");
    };
    assert_eq!(args.provider, "ollama");
    assert_eq!(args.model.as_deref(), Some("devstral-small-2"));
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:11434"));
    // The probe is meant for eval pipelines; JSON output is the default
    // surface so an aggregator doesn't have to opt in.
    assert!(args.json);
}

#[test]
fn test_parses_provider_tool_probe_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider-tool-probe",
        "ollama",
        "--model",
        "devstral-small-2",
        "--base-url",
        "http://127.0.0.1:11434",
        "--mode",
        "non-streaming",
        "--marker",
        "marker",
        "--response-fixture",
        "fixture.json",
    ]);

    let Command::ProviderToolProbe(args) = cli.command.unwrap() else {
        panic!("expected provider-tool-probe command");
    };
    assert_eq!(args.provider, "ollama");
    assert_eq!(args.model, "devstral-small-2");
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:11434"));
    assert!(matches!(args.mode, ProviderToolProbeModeArg::NonStreaming));
    assert_eq!(args.marker, "marker");
    assert_eq!(args.response_fixture, Some(PathBuf::from("fixture.json")));
    assert!(args.json);
}

#[test]
fn test_provider_model_completion_candidates_stay_permissive() {
    let cli = Cli::parse_from([
        "harn",
        "provider-ready",
        "custom-provider",
        "--model",
        "vendor/custom-model",
    ]);
    let Command::ProviderReady(args) = cli.command.unwrap() else {
        panic!("expected provider-ready command");
    };
    assert_eq!(args.provider, "custom-provider");
    assert_eq!(args.model.as_deref(), Some("vendor/custom-model"));

    let command = Cli::command();
    let provider_ready = command
        .find_subcommand("provider-ready")
        .expect("provider-ready subcommand");
    let provider_values: Vec<_> = provider_ready
        .get_arguments()
        .find(|arg| arg.get_id() == "provider")
        .expect("provider argument")
        .get_possible_values()
        .into_iter()
        .map(|value| value.get_name().to_string())
        .collect();
    assert!(provider_values.iter().any(|value| value == "openai"));

    let model_values: Vec<_> = provider_ready
        .get_arguments()
        .find(|arg| arg.get_id() == "model")
        .expect("model argument")
        .get_possible_values()
        .into_iter()
        .map(|value| value.get_name().to_string())
        .collect();
    assert!(model_values.iter().any(|value| value == "gpt-4o-mini"));
}

#[test]
fn test_completion_scripts_include_subcommands() {
    let mut command = Cli::command();
    let mut output = Vec::new();
    clap_complete::generate(
        clap_complete::Shell::Bash,
        &mut command,
        "harn",
        &mut output,
    );
    let script = String::from_utf8(output).expect("completion script should be utf-8");

    assert!(script.contains("completions"));
    assert!(script.contains("provider-ready"));
}

#[test]
fn test_parses_models_recommend_args() {
    let cli = Cli::parse_from(["harn", "models", "recommend", "--json"]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Recommend(recommend) = args.command else {
        panic!("expected models recommend command");
    };
    assert!(recommend.json);
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
fn test_parses_check_provider_matrix_args() {
    let cli = Cli::parse_from([
        "harn",
        "check",
        "--provider-matrix",
        "--format",
        "markdown",
        "--filter",
        "json-schema",
    ]);

    let Command::Check(args) = cli.command.unwrap() else {
        panic!("expected check command");
    };
    assert!(args.provider_matrix);
    assert_eq!(args.format, CheckOutputFormat::Markdown);
    assert_eq!(args.filter.as_deref(), Some("json-schema"));
    assert!(args.targets.is_empty());
}

#[test]
fn test_parses_providers_recommend_args() {
    let cli = Cli::parse_from([
        "harn",
        "providers",
        "recommend",
        "--input",
        "local_readiness.json",
        "--provider",
        "ollama",
        "--json",
    ]);

    let Command::Providers(args) = cli.command.unwrap() else {
        panic!("expected providers command");
    };
    let ProvidersCommand::Recommend(recommend) = args.command else {
        panic!("expected providers recommend command");
    };
    assert_eq!(recommend.input, Some(PathBuf::from("local_readiness.json")));
    assert_eq!(recommend.provider.as_deref(), Some("ollama"));
    assert!(recommend.json);
}

#[test]
fn test_parses_check_connector_matrix_args() {
    let cli = Cli::parse_from([
        "harn",
        "check",
        "--connector-matrix",
        "--format",
        "json",
        "--filter",
        "rate-limit",
        "fixtures/connectors",
    ]);

    let Command::Check(args) = cli.command.unwrap() else {
        panic!("expected check command");
    };
    assert!(args.connector_matrix);
    assert_eq!(args.format, CheckOutputFormat::Json);
    assert_eq!(args.filter.as_deref(), Some("rate-limit"));
    assert_eq!(args.targets, vec!["fixtures/connectors"]);
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
