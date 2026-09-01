use super::*;

#[test]
fn serve_acp_accepts_attach_mode_without_a_file() {
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
fn serve_a2a_keeps_the_legacy_port_default_when_bind_is_explicit() {
    let cli = Cli::parse_from([
        "harn",
        "serve",
        "a2a",
        "--bind",
        "0.0.0.0:3000",
        "agent.harn",
    ]);
    let Command::Serve(args) = cli.command.unwrap() else {
        panic!("expected serve command");
    };
    let crate::cli::ServeCommand::A2a(serve) = args.command else {
        panic!("expected serve a2a");
    };
    assert_eq!(serve.port, 8080);
}

#[test]
fn portal_accepts_an_explicit_false_open_flag() {
    let cli = Cli::parse_from(["harn", "portal", "--open", "false"]);
    let Command::Portal(args) = cli.command.unwrap() else {
        panic!("expected portal command");
    };
    assert!(!args.open);
}

#[test]
fn tool_new_and_skill_new_share_the_same_shape() {
    let cli = Cli::parse_from(["harn", "tool", "new", "acme-tool"]);
    let Command::Tool(args) = cli.command.unwrap() else {
        panic!("expected tool command");
    };
    let ToolCommand::New(tool) = args.command else {
        panic!("expected tool new command");
    };

    let cli = Cli::parse_from(["harn", "skill", "new", "deploy"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::New(skill) = args.command else {
        panic!("expected skill new alias");
    };
    assert_eq!(tool.description, skill.description);
}

#[test]
fn tool_schema_defaults_to_script_and_accepts_offline_exports() {
    let cli = Cli::parse_from(["harn", "tool", "schema", "server.harn"]);
    let Command::Tool(args) = cli.command.unwrap() else {
        panic!("expected tool command");
    };
    let ToolCommand::Schema(schema) = args.command else {
        panic!("expected tool schema command");
    };
    assert_eq!(schema.surface, ToolSchemaSurface::Script);

    let cli = Cli::parse_from([
        "harn",
        "tool",
        "schema",
        "server.harn",
        "--surface",
        "exports",
        "--pretty",
    ]);
    let Command::Tool(args) = cli.command.unwrap() else {
        panic!("expected tool command");
    };
    let ToolCommand::Schema(schema) = args.command else {
        panic!("expected tool schema command");
    };
    assert_eq!(schema.surface, ToolSchemaSurface::Exports);
    assert!(schema.pretty);
}

#[test]
fn local_stop_all_does_not_infer_a_provider() {
    let cli = Cli::parse_from(["harn", "local", "stop", "--all"]);
    let Command::Local(args) = cli.command.unwrap() else {
        panic!("expected local command");
    };
    let LocalCommand::Stop(args) = args.command else {
        panic!("expected local stop command");
    };
    assert!(args.provider.is_none());
}
