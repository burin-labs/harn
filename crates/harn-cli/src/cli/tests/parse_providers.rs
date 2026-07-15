use super::*;

#[test]
fn test_parses_provider_capabilities_audit_json() {
    let cli = Cli::parse_from(["harn", "provider", "capabilities", "audit", "--json"]);

    let Command::Provider(args) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Capabilities(capabilities) = args.command else {
        panic!("expected capabilities command");
    };
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
    let ProviderCommand::Capabilities(capabilities) = args.command else {
        panic!("expected capabilities command");
    };
    let ProviderCapabilitiesCommand::PromoteFromEval(promote) = capabilities.command else {
        panic!("expected promote-from-eval command");
    };
    assert_eq!(promote.overlay_path, PathBuf::from("overlay.toml"));
    assert_eq!(promote.catalog, PathBuf::from("custom-capabilities.toml"));
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
fn test_parses_mcp_call_command() {
    let cli = Cli::parse_from([
        "harn",
        "mcp",
        "call",
        "--tool",
        "harn.trigger.fire",
        "--arguments",
        r#"{"trigger_id":"cron-ok"}"#,
        "--progress-token",
        "token-1",
        "--",
        "harn",
        "mcp",
        "serve",
    ]);
    let Command::Mcp(args) = cli.command.unwrap() else {
        panic!("expected mcp command");
    };
    let McpCommand::Call(call) = args.command else {
        panic!("expected mcp call");
    };
    assert_eq!(call.tool, "harn.trigger.fire");
    assert_eq!(call.arguments, r#"{"trigger_id":"cron-ok"}"#);
    assert_eq!(call.progress_token.as_deref(), Some("token-1"));
    assert_eq!(call.command, vec!["harn", "mcp", "serve"]);
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
fn test_parses_models_info_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "info",
        "--verify",
        "--warm",
        "--keep-alive",
        "forever",
        "tog-gemma4-31b",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Info(args) = args.command else {
        panic!("expected models info");
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
fn test_parses_models_lora_inspect_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "inspect",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--name",
        "burin-tools",
        "--json",
        "./adapter",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Inspect(args) = args.command else {
        panic!("expected models lora inspect command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.name.as_deref(), Some("burin-tools"));
    assert_eq!(args.adapter, "./adapter");
    assert!(args.json);
}

#[test]
fn test_parses_models_lora_export_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "export",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--tool-format",
        "native",
        "--corpus",
        "./lora-corpus",
        "--out",
        "./data/tool-calls.jsonl",
        "--manifest",
        "./data/tool-calls.manifest.json",
        "--adapter-name",
        "burin-tools",
        "--chat-template",
        "gemma-4",
        "--default-split",
        "tune",
        "--default-license",
        "Apache-2.0",
        "--target-metadata",
        "lane=structured",
        "--modules-to-save",
        "embed_tokens,lm_head",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Export(args) = args.command else {
        panic!("expected models lora export command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.tool_format, "native");
    assert_eq!(args.corpus, "./lora-corpus");
    assert_eq!(
        args.out
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./data/tool-calls.jsonl")
    );
    assert_eq!(
        args.manifest
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./data/tool-calls.manifest.json")
    );
    assert_eq!(args.adapter_name.as_deref(), Some("burin-tools"));
    assert_eq!(args.chat_template.as_deref(), Some("gemma-4"));
    assert_eq!(args.default_split, "tune");
    assert_eq!(args.default_license, "Apache-2.0");
    assert_eq!(args.target_metadata, vec!["lane=structured"]);
    assert_eq!(args.modules_to_save, vec!["embed_tokens", "lm_head"]);
    assert!(args.json);
}

#[test]
fn test_parses_models_lora_manifest_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "manifest",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--tool-format",
        "json",
        "--dataset",
        "./data/tool-calls.jsonl",
        "--corpus",
        "./lora-corpus",
        "--export-manifest",
        "./data/tool-calls.manifest.json",
        "--out",
        "./adapters/burin-tools.manifest.json",
        "--adapter-name",
        "burin-tools",
        "--adapter-path",
        "./adapters/burin-tools",
        "--request-model",
        "burin-tools",
        "--chat-template",
        "harn_text_tool_calls_json_fences",
        "--trainer",
        "unsloth_sft",
        "--trainer-version",
        "unsloth-2026.7",
        "--method",
        "lora",
        "--rank",
        "32",
        "--alpha",
        "64",
        "--dropout",
        "0.1",
        "--training-run-id",
        "run-123",
        "--teacher",
        "dashscope/qwen3-coder-next",
        "--target-metadata",
        "lane=structured",
        "--modules-to-save",
        "embed_tokens",
        "--modules-to-save",
        "lm_head",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Manifest(args) = args.command else {
        panic!("expected models lora manifest command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.tool_format, "json");
    assert_eq!(
        args.dataset
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./data/tool-calls.jsonl")
    );
    assert_eq!(
        args.corpus
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./lora-corpus")
    );
    assert_eq!(
        args.export_manifest
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./data/tool-calls.manifest.json")
    );
    assert_eq!(
        args.out
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./adapters/burin-tools.manifest.json")
    );
    assert_eq!(args.adapter_name.as_deref(), Some("burin-tools"));
    assert_eq!(args.adapter_path.as_deref(), Some("./adapters/burin-tools"));
    assert_eq!(args.request_model.as_deref(), Some("burin-tools"));
    assert_eq!(
        args.chat_template.as_deref(),
        Some("harn_text_tool_calls_json_fences")
    );
    assert_eq!(args.trainer, "unsloth_sft");
    assert_eq!(args.trainer_version.as_deref(), Some("unsloth-2026.7"));
    assert_eq!(args.method, "lora");
    assert_eq!(args.rank, 32);
    assert_eq!(args.alpha, Some(64));
    assert_eq!(args.dropout, 0.1);
    assert_eq!(args.training_run_id.as_deref(), Some("run-123"));
    assert_eq!(args.teacher.as_deref(), Some("dashscope/qwen3-coder-next"));
    assert_eq!(args.target_metadata, vec!["lane=structured"]);
    assert_eq!(args.modules_to_save, vec!["embed_tokens", "lm_head"]);
    assert!(args.json);
}

#[test]
fn test_parses_models_lora_train_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "train",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--tool-format",
        "json",
        "--dataset",
        "./data/tool-calls.jsonl",
        "--corpus",
        "./lora-corpus",
        "--export-manifest",
        "./data/tool-calls.manifest.json",
        "--output-dir",
        "./adapters/burin-tools",
        "--receipt-out",
        "./adapters/train.receipt.json",
        "--adapter-name",
        "burin-tools",
        "--request-model",
        "burin-tools",
        "--chat-template",
        "harn_text_tool_calls_json_fences",
        "--trainer",
        "unsloth_sft",
        "--trainer-version",
        "unsloth-2026.7",
        "--method",
        "lora",
        "--rank",
        "32",
        "--alpha",
        "64",
        "--dropout",
        "0.1",
        "--max-seq-length",
        "8192",
        "--teacher",
        "dashscope/qwen3-coder-next",
        "--target-metadata",
        "lane=structured",
        "--modules-to-save",
        "embed_tokens,lm_head",
        "--backend-cwd",
        "./scripts/train-lora-gemma4",
        "--backend-result-out",
        "./adapters/backend.result.json",
        "--execute",
        "--json",
        "--",
        "uv",
        "run",
        "python",
        "train.py",
        "config/e4b.yaml",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Train(args) = args.command else {
        panic!("expected models lora train command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.tool_format, "json");
    assert_eq!(
        args.dataset.display().to_string(),
        "./data/tool-calls.jsonl"
    );
    assert_eq!(
        args.corpus
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./lora-corpus")
    );
    assert_eq!(
        args.export_manifest
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./data/tool-calls.manifest.json")
    );
    assert_eq!(
        args.output_dir.display().to_string(),
        "./adapters/burin-tools"
    );
    assert_eq!(
        args.receipt_out
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./adapters/train.receipt.json")
    );
    assert_eq!(args.adapter_name.as_deref(), Some("burin-tools"));
    assert_eq!(args.request_model.as_deref(), Some("burin-tools"));
    assert_eq!(
        args.chat_template.as_deref(),
        Some("harn_text_tool_calls_json_fences")
    );
    assert_eq!(args.trainer, "unsloth_sft");
    assert_eq!(args.trainer_version.as_deref(), Some("unsloth-2026.7"));
    assert_eq!(args.method, "lora");
    assert_eq!(args.rank, 32);
    assert_eq!(args.alpha, Some(64));
    assert_eq!(args.dropout, 0.1);
    assert_eq!(args.max_seq_length, Some(8192));
    assert_eq!(args.teacher.as_deref(), Some("dashscope/qwen3-coder-next"));
    assert_eq!(args.target_metadata, vec!["lane=structured"]);
    assert_eq!(args.modules_to_save, vec!["embed_tokens", "lm_head"]);
    assert_eq!(args.backend_recipe, "explicit_argv");
    assert!(args.backend_runner.is_empty());
    assert!(args.backend_script.is_none());
    assert!(args.backend_config.is_none());
    assert_eq!(
        args.backend_result_out
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./adapters/backend.result.json")
    );
    assert_eq!(
        args.backend_cwd
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./scripts/train-lora-gemma4")
    );
    assert!(args.execute);
    assert!(args.json);
    assert_eq!(
        args.backend_argv,
        vec!["uv", "run", "python", "train.py", "config/e4b.yaml"]
    );
}

#[test]
fn test_parses_models_lora_promote_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "promote",
        "--manifest",
        "./adapters/burin-tools.manifest.json",
        "--probe-root",
        "./promotions/probes",
        "--base-probe-root",
        "./promotions/base-probes",
        "--out",
        "./promotions/probe-matrix.receipt.json",
        "--check",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Promote(args) = args.command else {
        panic!("expected models lora promote command");
    };
    assert_eq!(
        args.manifest.display().to_string(),
        "./adapters/burin-tools.manifest.json"
    );
    assert_eq!(args.probe_root.display().to_string(), "./promotions/probes");
    assert_eq!(
        args.base_probe_root.as_deref(),
        Some(std::path::Path::new("./promotions/base-probes"))
    );
    assert_eq!(
        args.out
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("./promotions/probe-matrix.receipt.json")
    );
    assert!(args.check);
    assert!(args.json);
}

#[test]
fn test_parses_models_lora_train_recipe_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "train",
        "--base",
        "local-gemma4-e4b",
        "--dataset",
        "./data/tool-calls.jsonl",
        "--output-dir",
        "./adapters/burin-tools",
        "--backend-recipe",
        "harn_lora_sft_v1",
        "--backend-runner",
        "uv,run,python",
        "--backend-script",
        "train.py",
        "--backend-config",
        "config/e4b.yaml",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Train(args) = args.command else {
        panic!("expected models lora train command");
    };
    assert_eq!(args.backend_recipe, "harn_lora_sft_v1");
    assert_eq!(args.backend_runner, vec!["uv", "run", "python"]);
    assert_eq!(
        args.backend_script
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("train.py")
    );
    assert_eq!(
        args.backend_config
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref(),
        Some("config/e4b.yaml")
    );
    assert!(args.backend_argv.is_empty());
}

#[test]
fn test_models_lora_train_defaults_to_external_trainer_contract() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "train",
        "--base",
        "local-gemma4-e4b",
        "--dataset",
        "./data/tool-calls.jsonl",
        "--output-dir",
        "./adapters/burin-tools",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Train(args) = args.command else {
        panic!("expected models lora train command");
    };
    assert_eq!(args.trainer, "external_sft_trainer");
    assert!(args.modules_to_save.is_empty());
    assert_eq!(args.backend_recipe, "explicit_argv");
    assert!(args.backend_runner.is_empty());
    assert!(args.backend_script.is_none());
    assert!(args.backend_config.is_none());
}

#[test]
fn test_parses_models_lora_plan_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "plan",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--tool-format",
        "json",
        "--corpus",
        "./lora-corpus",
        "--teacher",
        "dashscope/qwen3-coder-next",
        "--corpus-strategy",
        "refresh",
        "--method",
        "lora",
        "--trainer",
        "unsloth_sft",
        "--rank",
        "32",
        "--alpha",
        "64",
        "--dropout",
        "0.1",
        "--modules-to-save",
        "embed_tokens,lm_head",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Plan(args) = args.command else {
        panic!("expected models lora plan command");
    };
    assert_eq!(args.base_model, "local-gemma4-e4b");
    assert_eq!(args.provider.as_deref(), Some("vllm"));
    assert_eq!(args.tool_format, "json");
    assert_eq!(args.corpus.as_deref(), Some("./lora-corpus"));
    assert_eq!(args.teacher.as_deref(), Some("dashscope/qwen3-coder-next"));
    assert_eq!(args.corpus_strategy, "refresh");
    assert_eq!(args.method, "lora");
    assert_eq!(args.trainer, "unsloth_sft");
    assert_eq!(args.rank, 32);
    assert_eq!(args.alpha, Some(64));
    assert_eq!(args.dropout, 0.1);
    assert_eq!(args.modules_to_save, vec!["embed_tokens", "lm_head"]);
    assert!(args.json);
}

#[test]
fn test_models_lora_plan_defaults_to_external_trainer_contract() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "plan",
        "--base",
        "local-gemma4-e4b",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Lora(args) = args.command else {
        panic!("expected models lora command");
    };
    let ModelsLoraCommand::Plan(args) = args.command else {
        panic!("expected models lora plan command");
    };
    assert_eq!(args.trainer, "external_sft_trainer");
    assert!(args.modules_to_save.is_empty());
}

#[test]
fn test_parses_models_batch_plan_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "plan",
        "--provider",
        "openai",
        "--model",
        "gpt-4o-mini",
        "--workload",
        "eval",
        "--min-discount-percent",
        "50",
        "--max-turnaround-hours",
        "24",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Plan(args) = args.command else {
        panic!("expected models batch plan command");
    };
    assert_eq!(args.provider.as_deref(), Some("openai"));
    assert_eq!(args.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(args.workload, "eval");
    assert_eq!(args.min_discount_percent, Some(50));
    assert_eq!(args.max_turnaround_hours, Some(24));
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_manifest_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "manifest",
        "--requests",
        "requests.jsonl",
        "--out",
        "manifest.json",
        "--provider",
        "openai",
        "--model",
        "gpt-4o-mini",
        "--workload",
        "eval",
        "--tool-format",
        "json",
        "--id-prefix",
        "eval-holdout",
        "--min-discount-percent",
        "50",
        "--max-turnaround-hours",
        "24",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Manifest(args) = args.command else {
        panic!("expected models batch manifest command");
    };
    assert_eq!(args.requests, std::path::PathBuf::from("requests.jsonl"));
    assert_eq!(args.out, std::path::PathBuf::from("manifest.json"));
    assert_eq!(args.provider.as_deref(), Some("openai"));
    assert_eq!(args.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(args.workload, "eval");
    assert_eq!(args.tool_format, "json");
    assert_eq!(args.id_prefix, "eval-holdout");
    assert_eq!(args.min_discount_percent, Some(50));
    assert_eq!(args.max_turnaround_hours, Some(24));
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_prepare_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "prepare",
        "--manifest",
        "batch-manifest.json",
        "--out-dir",
        ".harn/batches/ready",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Prepare(args) = args.command else {
        panic!("expected models batch prepare command");
    };
    assert_eq!(
        args.manifest,
        std::path::PathBuf::from("batch-manifest.json")
    );
    assert_eq!(
        args.out_dir,
        std::path::PathBuf::from(".harn/batches/ready")
    );
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_submit_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "submit",
        "--receipt",
        ".harn/batches/ready/receipt.json",
        "--out",
        ".harn/batches/submission.json",
        "--dry-run",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Submit(args) = args.command else {
        panic!("expected models batch submit command");
    };
    assert_eq!(
        args.receipt,
        std::path::PathBuf::from(".harn/batches/ready/receipt.json")
    );
    assert_eq!(
        args.out,
        std::path::PathBuf::from(".harn/batches/submission.json")
    );
    assert!(args.dry_run);
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_status_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "status",
        "--submission",
        ".harn/batches/submission.json",
        "--out",
        ".harn/batches/status.json",
        "--dry-run",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Status(args) = args.command else {
        panic!("expected models batch status command");
    };
    assert_eq!(
        args.submission,
        std::path::PathBuf::from(".harn/batches/submission.json")
    );
    assert_eq!(
        args.out,
        std::path::PathBuf::from(".harn/batches/status.json")
    );
    assert!(args.dry_run);
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_cancel_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "cancel",
        "--receipt",
        ".harn/batches/status.json",
        "--out",
        ".harn/batches/cancel.json",
        "--dry-run",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Cancel(args) = args.command else {
        panic!("expected models batch cancel command");
    };
    assert_eq!(
        args.receipt,
        std::path::PathBuf::from(".harn/batches/status.json")
    );
    assert_eq!(
        args.out,
        std::path::PathBuf::from(".harn/batches/cancel.json")
    );
    assert!(args.dry_run);
    assert!(args.json);
}

#[test]
fn test_parses_models_batch_download_args() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "batch",
        "download",
        "--status",
        ".harn/batches/status.json",
        "--out-dir",
        ".harn/batches/results",
        "--max-bytes",
        "1048576",
        "--dry-run",
        "--json",
    ]);

    let Command::Models(args) = cli.command.unwrap() else {
        panic!("expected models command");
    };
    let ModelsCommand::Batch(args) = args.command else {
        panic!("expected models batch command");
    };
    let ModelsBatchCommand::Download(args) = args.command else {
        panic!("expected models batch download command");
    };
    assert_eq!(
        args.status,
        std::path::PathBuf::from(".harn/batches/status.json")
    );
    assert_eq!(
        args.out_dir,
        std::path::PathBuf::from(".harn/batches/results")
    );
    assert_eq!(args.max_bytes, 1_048_576);
    assert!(args.dry_run);
    assert!(args.json);
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
    assert!(args.check);
    assert!(args.json);
}

fn parse_provider_catalog(args: &[&str]) -> ProviderCatalogCommand {
    let mut argv = vec!["harn", "provider", "catalog"];
    argv.extend_from_slice(args);
    let cli = Cli::parse_from(argv);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Catalog(catalog) = provider.command else {
        panic!("expected provider catalog command");
    };
    catalog.command
}

#[test]
fn test_parses_providers_refresh_args() {
    let ProviderCatalogCommand::Refresh(args) = parse_provider_catalog(&[
        "refresh",
        "--live",
        "--check",
        "--script",
        "scripts/update_provider_catalog.harn",
    ]) else {
        panic!("expected provider catalog refresh command");
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
    let ProviderCatalogCommand::Validate(args) =
        parse_provider_catalog(&["validate", "--overlay", "providers.local.toml", "--json"])
    else {
        panic!("expected provider catalog validate command");
    };
    assert_eq!(
        args.overlay.as_deref(),
        Some(std::path::Path::new("providers.local.toml"))
    );
    assert!(args.json);
}

#[test]
fn test_parses_providers_export_args() {
    let ProviderCatalogCommand::Export(args) =
        parse_provider_catalog(&["export", "--output-dir", "tmp/catalog", "--check"])
    else {
        panic!("expected provider catalog export command");
    };
    assert_eq!(args.output_dir, std::path::PathBuf::from("tmp/catalog"));
    assert!(args.check);
}

#[test]
fn test_parses_providers_generate_args() {
    let ProviderCatalogCommand::Generate(args) = parse_provider_catalog(&[
        "generate",
        "--source-dir",
        "tmp/catalog_sources",
        "--capability-source-dir",
        "tmp/capability_sources",
        "--providers-output",
        "tmp/providers.toml",
        "--capabilities-output",
        "tmp/capabilities.toml",
        "--artifact-dir",
        "tmp/catalog",
        "--check",
    ]) else {
        panic!("expected provider catalog generate command");
    };
    assert_eq!(
        args.source_dir,
        std::path::PathBuf::from("tmp/catalog_sources")
    );
    assert_eq!(
        args.capability_source_dir,
        std::path::PathBuf::from("tmp/capability_sources")
    );
    assert_eq!(
        args.providers_output,
        std::path::PathBuf::from("tmp/providers.toml")
    );
    assert_eq!(
        args.capabilities_output,
        std::path::PathBuf::from("tmp/capabilities.toml")
    );
    assert_eq!(args.artifact_dir, std::path::PathBuf::from("tmp/catalog"));
    assert!(args.check);
}

#[test]
fn test_parses_providers_matrix_args() {
    let ProviderCatalogCommand::Matrix(args) = parse_provider_catalog(&[
        "matrix",
        "--output",
        "tmp/provider-matrix.md",
        "--check",
        "--stdout",
        "--filter",
        "native_tools",
    ]) else {
        panic!("expected provider catalog matrix command");
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
    let ProviderCatalogCommand::Support(args) = parse_provider_catalog(&[
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
    ]) else {
        panic!("expected provider catalog support command");
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
    let ProviderCatalogCommand::Show(args) =
        parse_provider_catalog(&["show", "--available-only", "--refresh"])
    else {
        panic!("expected provider catalog show command");
    };
    assert!(args.available_only);
    assert!(args.refresh);
}

#[test]
fn test_parses_provider_ready_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "ready",
        "mlx",
        "--model",
        "mlx-qwen3.6",
        "--base-url",
        "http://127.0.0.1:8002",
        "--json",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Ready(args) = provider.command else {
        panic!("expected provider ready command");
    };
    assert_eq!(args.provider, "mlx");
    assert_eq!(args.model.as_deref(), Some("mlx-qwen3.6"));
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:8002"));
    assert!(args.json);
}

#[test]
fn test_parses_provider_probe_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "probe",
        "ollama",
        "--model",
        "devstral-small-2",
        "--base-url",
        "http://127.0.0.1:11434",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Probe(args) = provider.command else {
        panic!("expected provider probe command");
    };
    assert_eq!(args.provider, "ollama");
    assert_eq!(args.model.as_deref(), Some("devstral-small-2"));
    assert_eq!(args.base_url.as_deref(), Some("http://127.0.0.1:11434"));
    assert!(args.json);
}

#[test]
fn test_parses_provider_tool_probe_args() {
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
        "large_string_argument",
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
        ProviderToolProbeCaseArg::LargeStringArgument
    ));
    assert_eq!(args.marker, "marker");
    assert_eq!(args.repeat, 5);
    assert_eq!(args.response_fixture, Some(PathBuf::from("fixture.json")));
    assert!(!args.dry_run_request);
    assert!(args.json);
}

#[test]
fn test_parses_provider_tool_probe_audit_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-probe-audit",
        "--mode",
        "streaming",
        "--case",
        "large_string_argument",
        "--json=false",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolProbeAudit(args) = provider.command else {
        panic!("expected provider tool-probe-audit command");
    };
    assert!(matches!(args.mode, ProviderToolProbeModeArg::Streaming));
    assert_eq!(args.probe_cases.len(), 1);
    assert!(matches!(
        args.probe_cases[0],
        ProviderToolProbeCaseArg::LargeStringArgument
    ));
    assert!(!args.json);
}

#[test]
fn test_parses_provider_tool_scorecard_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-scorecard",
        "--tool-probe-report",
        "anthropic.json",
        "--tool-probe-report",
        "fireworks.json",
        "--json=false",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolScorecard(args) = provider.command else {
        panic!("expected provider tool-scorecard command");
    };
    assert_eq!(
        args.tool_probe_reports,
        vec![
            PathBuf::from("anthropic.json"),
            PathBuf::from("fireworks.json")
        ]
    );
    assert!(!args.plan_from_catalog);
    assert!(args.routes.is_empty());
    assert!(!args.include_batch_manifest);
    assert!(!args.markdown);
    assert!(!args.json);
}

#[test]
fn test_parses_provider_tool_scorecard_plan_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
        "--include-batch-manifest",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolScorecard(args) = provider.command else {
        panic!("expected provider tool-scorecard command");
    };
    assert!(args.plan_from_catalog);
    assert_eq!(args.routes, vec!["anthropic:claude-sonnet-5"]);
    assert!(args.include_batch_manifest);
    assert!(!args.markdown);
    assert!(args.tool_probe_reports.is_empty());
    assert!(args.json);
}

#[test]
fn test_parses_provider_tool_scorecard_markdown_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
        "--markdown",
    ]);

    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolScorecard(args) = provider.command else {
        panic!("expected provider tool-scorecard command");
    };
    assert!(args.plan_from_catalog);
    assert_eq!(args.routes, vec!["anthropic:claude-sonnet-5"]);
    assert!(args.markdown);
}

#[test]
fn test_provider_model_completion_candidates_stay_permissive() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "ready",
        "custom-provider",
        "--model",
        "vendor/custom-model",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Ready(args) = provider.command else {
        panic!("expected provider ready command");
    };
    assert_eq!(args.provider, "custom-provider");
    assert_eq!(args.model.as_deref(), Some("vendor/custom-model"));

    let command = Cli::command();
    let provider_ready = command
        .find_subcommand("provider")
        .expect("provider subcommand")
        .find_subcommand("ready")
        .expect("provider ready subcommand");
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
fn test_parses_providers_recommend_args() {
    let ProviderCatalogCommand::Recommend(recommend) = parse_provider_catalog(&[
        "recommend",
        "--input",
        "local_readiness.json",
        "--provider",
        "ollama",
        "--json",
    ]) else {
        panic!("expected provider catalog recommend command");
    };
    assert_eq!(recommend.input, Some(PathBuf::from("local_readiness.json")));
    assert_eq!(recommend.provider.as_deref(), Some("ollama"));
    assert!(recommend.json);
}

#[test]
fn test_parses_provider_dispatch_explain_args() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "dispatch-explain",
        "anthropic",
        "claude-sonnet-4-6",
        "--thinking",
        "--tool-format",
        "native",
        "--json",
    ]);

    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::DispatchExplain(args) = provider.command else {
        panic!("expected provider dispatch-explain command");
    };
    assert_eq!(args.provider, "anthropic");
    assert_eq!(args.model, "claude-sonnet-4-6");
    assert!(args.thinking);
    assert_eq!(args.tool_format.as_deref(), Some("native"));
    assert!(args.json);
}
