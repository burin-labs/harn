use super::*;

#[test]
fn lint_parses_trusted_host_dispatch() {
    let cli = Cli::parse_from(["harn", "lint", "--trusted-host-dispatch", "routes/"]);
    let Command::Lint(args) = cli.command.unwrap() else {
        panic!("expected lint command");
    };
    assert!(args.trusted_host_dispatch);

    let plain = Cli::parse_from(["harn", "lint", "routes/"]);
    let Command::Lint(plain) = plain.command.unwrap() else {
        panic!("expected lint command");
    };
    assert!(!plain.trusted_host_dispatch, "unprivileged by default");
}

#[test]
fn lint_changed_revision_mode_does_not_require_targets() {
    let cli = Cli::parse_from(["harn", "lint", "--changed-from", "origin/main"]);
    let Command::Lint(args) = cli.command.unwrap() else {
        panic!("expected lint command");
    };
    assert!(args.targets.is_empty());
}

#[test]
fn lint_changed_revision_mode_rejects_fix_and_explicit_targets() {
    assert!(Cli::try_parse_from(["harn", "lint", "--changed-from", "HEAD^", "--fix"]).is_err());
    assert!(Cli::try_parse_from(["harn", "lint", "--changed-from", "HEAD^", "main.harn"]).is_err());
    assert!(Cli::try_parse_from(["harn", "lint", "--changed-to", "HEAD", "main.harn"]).is_err());
}

#[test]
fn new_command_distinguishes_project_templates_from_package_names() {
    let project = Cli::parse_from(["harn", "new", "review-bot", "--template", "agent"]);
    let Command::New(project) = project.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(project.second, None);
    assert_eq!(project.template, Some(ProjectTemplate::Agent));

    let package = Cli::parse_from(["harn", "new", "package", "acme-lib"]);
    let Command::New(package) = package.command.unwrap() else {
        panic!("expected new command");
    };
    assert_eq!(package.second.as_deref(), Some("acme-lib"));
    assert_eq!(package.template, None);
}

#[test]
fn completion_scripts_include_nested_subcommands() {
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
fn provider_matrix_mode_does_not_require_file_targets() {
    let cli = Cli::parse_from(["harn", "check", "--provider-matrix"]);
    let Command::Check(args) = cli.command.unwrap() else {
        panic!("expected check command");
    };
    assert!(args.targets.is_empty());
}
