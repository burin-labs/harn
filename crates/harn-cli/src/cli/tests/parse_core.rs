use super::*;

#[test]
fn test_parses_public_api_type_lint_override() {
    let cli = Cli::parse_from([
        "harn",
        "lint",
        "--require-public-api-types",
        "--json",
        "api.harn",
    ]);
    let Command::Lint(args) = cli.command.unwrap() else {
        panic!("expected lint command");
    };
    assert!(args.require_public_api_types);
    assert!(args.json);
    assert_eq!(args.targets, ["api.harn"]);
}

#[test]
fn lint_parses_changed_revision_mode_without_targets() {
    let cli = Cli::parse_from([
        "harn",
        "lint",
        "--strict",
        "--json",
        "--changed-from",
        "origin/main",
        "--changed-to",
        "HEAD",
    ]);
    let Command::Lint(args) = cli.command.unwrap() else {
        panic!("expected lint command");
    };
    assert_eq!(args.changed_from.as_deref(), Some("origin/main"));
    assert_eq!(args.changed_to.as_deref(), Some("HEAD"));
    assert!(args.targets.is_empty());
    assert!(args.strict);
    assert!(args.json);
}

#[test]
fn lint_changed_revision_mode_rejects_fix_and_explicit_targets() {
    assert!(Cli::try_parse_from(["harn", "lint", "--changed-from", "HEAD^", "--fix",]).is_err());
    assert!(
        Cli::try_parse_from(["harn", "lint", "--changed-from", "HEAD^", "main.harn",]).is_err()
    );
    assert!(Cli::try_parse_from(["harn", "lint", "--changed-to", "HEAD", "main.harn"]).is_err());
}

#[test]
fn check_parses_independent_fixture_mode() {
    let cli = Cli::parse_from(["harn", "check", "--json", "--independent", "fixtures"]);
    let Command::Check(args) = cli.command.unwrap() else {
        panic!("expected check command");
    };
    assert!(args.json);
    assert!(args.independent);
    assert_eq!(args.targets, ["fixtures"]);
}

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
fn test_parses_test_shard_flags() {
    let cli = Cli::parse_from([
        "harn",
        "test",
        "--parallel",
        "--shard-index",
        "2",
        "--shard-total",
        "4",
        "tests/",
    ]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert_eq!(args.shard_index, Some(2));
    assert_eq!(args.shard_total, Some(4));
    assert_eq!(args.target.as_deref(), Some("tests/"));
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
fn test_parses_completion_args() {
    let cli = Cli::parse_from(["harn", "completion", "zsh"]);

    let Command::Completion(args) = cli.command.unwrap() else {
        panic!("expected completion command");
    };
    assert_eq!(args.shell, CompletionShell::Zsh);
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

    assert!(script.contains("completion"));
    assert!(script.contains("tool-probe"));
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
fn test_parses_check_strict_args() {
    let cli = Cli::parse_from(["harn", "check", "--strict", "--strict-types", "main.harn"]);

    let Command::Check(args) = cli.command.unwrap() else {
        panic!("expected check command");
    };
    assert!(args.strict);
    assert!(args.strict_types);
    assert_eq!(args.targets, vec!["main.harn"]);
}
