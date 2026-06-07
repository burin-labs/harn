use super::*;

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
fn test_parses_local_launch_args() {
    let cli = Cli::parse_from([
        "harn",
        "local",
        "launch",
        "local-qwen3.6",
        "--provider",
        "llamacpp",
        "--model-path",
        "/models/qwen.gguf",
        "--server-command",
        "llama-server",
        "--host",
        "127.0.0.1",
        "--port",
        "8001",
        "--ctx",
        "8192",
        "--parallel",
        "1",
        "--gpu-layers",
        "auto",
        "--cache-type-k",
        "q8_0",
        "--cache-type-v",
        "q8_0",
        "--cache-ram",
        "0",
        "--reasoning",
        "off",
        "--reasoning-format",
        "deepseek",
        "--flash-attn",
        "on",
        "--jinja",
        "--metrics",
        "--server-arg",
        "--no-warmup",
        "--timeout-secs",
        "60",
        "--log",
        "/tmp/qwen.log",
        "--no-evict",
        "--json",
    ]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::Launch(args) = args.command else {
        panic!("expected local launch command");
    };
    assert_eq!(args.model, "local-qwen3.6");
    assert_eq!(args.provider.as_deref(), Some("llamacpp"));
    assert_eq!(args.model_source.as_deref(), Some("/models/qwen.gguf"));
    assert_eq!(args.server_command.as_deref(), Some("llama-server"));
    assert_eq!(args.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(args.port, Some(8001));
    assert_eq!(args.ctx, Some(8192));
    assert_eq!(args.parallel, 1);
    assert_eq!(args.gpu_layers, "auto");
    assert_eq!(args.cache_type_k.as_deref(), Some("q8_0"));
    assert_eq!(args.cache_type_v.as_deref(), Some("q8_0"));
    assert_eq!(args.cache_ram, Some(0));
    assert_eq!(args.reasoning.as_deref(), Some("off"));
    assert_eq!(args.reasoning_format.as_deref(), Some("deepseek"));
    assert_eq!(args.flash_attn.as_deref(), Some("on"));
    assert!(args.jinja);
    assert!(args.metrics);
    assert_eq!(args.server_args, vec!["--no-warmup".to_string()]);
    assert_eq!(args.timeout_secs, 60);
    assert_eq!(args.log, Some(PathBuf::from("/tmp/qwen.log")));
    assert!(args.no_evict);
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
