use super::*;
use crate::cli::PersonaTemplateKind;

#[test]
fn supervised_cargo_lease_defaults_to_ci_verify_priority() {
    let cli = Cli::parse_from([
        "harn",
        "host",
        "lease",
        "run",
        "cargo",
        "--owner",
        "codex-0",
        "--workspace",
        "/workspace",
        "--target-dir",
        "/target",
        "--",
        "check",
    ]);
    let Command::Host(args) = cli.command.unwrap() else {
        panic!("expected host command");
    };
    let HostCommand::Lease(lease) = args.command;
    let HostLeaseCommand::Run(run) = lease.command else {
        panic!("expected host lease run command");
    };
    let HostLeaseRunCommand::Cargo(cargo) = run.command;
    assert!(matches!(
        cargo.priority_class,
        HostLeasePriorityArg::CiVerify
    ));
}

#[test]
fn replay_sources_are_distinct_and_have_usable_defaults() {
    let cli = Cli::parse_from(["harn", "replay", "run.json"]);
    let Command::Replay(path) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert!(path.fixture.is_none());
    assert!(path.session_id.is_none());
    assert_eq!(path.runs, 1);

    let cli = Cli::parse_from(["harn", "replay", "--fixture", "trace.json"]);
    let Command::Replay(fixture) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert!(fixture.path.is_none());
    assert!(fixture.session_id.is_none());

    let cli = Cli::parse_from([
        "harn",
        "replay",
        "--session-id",
        "session-123",
        "--events-db",
        "events.sqlite",
    ]);
    let Command::Replay(session) = cli.command.unwrap() else {
        panic!("expected replay command");
    };
    assert!(session.path.is_none());
    assert!(session.fixture.is_none());
    assert!(session.counterfactual.is_empty());
}

#[test]
fn session_view_fixture_check_does_not_write() {
    let cli = Cli::parse_from(["harn", "session", "view-fixtures", "--check"]);
    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::ViewFixtures(fixtures) = args.command else {
        panic!("expected session view-fixtures");
    };
    assert!(!fixtures.write);
}

#[test]
fn merge_captain_keeps_format_flags_and_input_modes_distinct() {
    let cli = Cli::parse_from([
        "harn",
        "merge-captain",
        "ladder",
        "eval.toml",
        "--format",
        "json",
    ]);
    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let MergeCaptainCommand::Ladder(format) = args.command else {
        panic!("expected ladder command");
    };
    assert!(!format.json);
    assert!(matches!(
        format.format,
        crate::cli::MergeCaptainLadderFormat::Json
    ));

    let cli = Cli::parse_from(["harn", "merge-captain", "ladder", "eval.toml", "--json"]);
    let Command::MergeCaptain(args) = cli.command.unwrap() else {
        panic!("expected merge-captain command");
    };
    let MergeCaptainCommand::Ladder(alias) = args.command else {
        panic!("expected ladder command");
    };
    assert!(alias.json);
    assert!(matches!(
        alias.format,
        crate::cli::MergeCaptainLadderFormat::Text
    ));

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
    let MergeCaptainCommand::Iterate(diff) = args.command else {
        panic!("expected iterate command");
    };
    assert!(diff.manifest.is_none());
}

#[test]
fn persona_new_keeps_manual_and_prompt_inputs_distinct() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "new",
        "incident_triager",
        "--template",
        "hybrid-classify-then-act",
    ]);
    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::New(manual) = *args.command else {
        panic!("expected persona new command");
    };
    assert_eq!(
        manual.template,
        Some(PersonaTemplateKind::HybridClassifyThenAct)
    );
    assert!(manual.from_prompt.is_none());

    let cli = Cli::parse_from([
        "harn",
        "persona",
        "new",
        "--from-prompt",
        "Digest replies.",
        "--name",
        "digest",
    ]);
    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::New(prompt) = *args.command else {
        panic!("expected persona new command");
    };
    assert!(prompt.name.is_none());
    assert!(prompt.template.is_none());
    assert_eq!(prompt.prompt_name.as_deref(), Some("digest"));
}

#[test]
fn persona_prompt_token_ceiling_is_hard() {
    let error = Cli::try_parse_from([
        "harn",
        "persona",
        "compile-prompt",
        "--prompt",
        "Compile this.",
        "--max-tokens",
        "1201",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn persona_materialize_keeps_blueprint_and_receipt_inputs_distinct() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "materialize",
        "--blueprint",
        "blueprint.json",
    ]);
    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::Materialize(blueprint) = *args.command else {
        panic!("expected persona materialize command");
    };
    assert!(blueprint.compile_receipt.is_none());
    assert!(!blueprint.activate);
    assert!(!blueprint.json);

    let cli = Cli::parse_from([
        "harn",
        "persona",
        "materialize",
        "--compile-receipt",
        "receipt.json",
    ]);
    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::Materialize(receipt) = *args.command else {
        panic!("expected persona materialize command");
    };
    assert!(receipt.blueprint.is_none());
}

#[test]
fn persona_materialize_requires_exactly_one_input() {
    assert_eq!(
        Cli::try_parse_from(["harn", "persona", "materialize"])
            .unwrap_err()
            .kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert_eq!(
        Cli::try_parse_from([
            "harn",
            "persona",
            "materialize",
            "--blueprint",
            "blueprint.json",
            "--compile-receipt",
            "receipt.json",
        ])
        .unwrap_err()
        .kind(),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn persona_materialize_apply_requires_manifest_and_json() {
    for args in [
        vec![
            "harn",
            "persona",
            "materialize",
            "--compile-receipt",
            "receipt.json",
            "--activate",
            "--json",
        ],
        vec![
            "harn",
            "persona",
            "materialize",
            "--compile-receipt",
            "receipt.json",
            "--manifest",
            "harn.toml",
            "--activate",
        ],
    ] {
        assert_eq!(
            Cli::try_parse_from(args).unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }
}

#[test]
fn persona_activation_rejects_unenforced_or_conflicting_flags() {
    assert_eq!(
        Cli::try_parse_from([
            "harn",
            "persona",
            "activate",
            "reviewer",
            "--daily-usd",
            "5",
        ])
        .unwrap_err()
        .kind(),
        clap::error::ErrorKind::UnknownArgument
    );
    assert_eq!(
        Cli::try_parse_from([
            "harn",
            "persona",
            "activate",
            "reviewer",
            "--tool",
            "filesystem",
            "--no-tools",
        ])
        .unwrap_err()
        .kind(),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn trigger_bulk_cancel_does_not_infer_an_event_id() {
    let cli = Cli::parse_from([
        "harn",
        "trigger",
        "cancel",
        "--where",
        "attempt.handler == 'risky'",
        "--dry-run",
    ]);
    let Command::Trigger(args) = cli.command.unwrap() else {
        panic!("expected trigger command");
    };
    let TriggerCommand::Cancel(cancel) = args.command else {
        panic!("expected trigger cancel");
    };
    assert!(cancel.event_id.is_none());
}

#[test]
fn trigger_replay_by_event_does_not_infer_a_filter() {
    let cli = Cli::parse_from(["harn", "trigger", "replay", "trigger_evt_123"]);
    let Command::Trigger(args) = cli.command.unwrap() else {
        panic!("expected trigger command");
    };
    let TriggerCommand::Replay(replay) = args.command else {
        panic!("expected trigger replay");
    };
    assert!(replay.where_expr.is_none());
}

#[test]
fn orchestrator_serve_container_aliases_map_to_local_state() {
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
fn bare_orchestrator_queue_has_no_implicit_operation() {
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
    assert!(queue.command.is_none());
}

#[test]
fn orchestrator_durations_are_normalized_once_at_the_cli_boundary() {
    let cli = Cli::parse_from([
        "harn",
        "orchestrator",
        "queue",
        "--state-dir",
        "state/orchestrator",
        "drain",
        "triage",
        "--claim-ttl",
        "30s",
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
    assert_eq!(drain.claim_ttl, StdDuration::from_secs(30));

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
    ]);
    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Recover(recover) = args.command else {
        panic!("expected orchestrator recover");
    };
    assert_eq!(recover.envelope_age, StdDuration::from_mins(15));
    assert!(!recover.yes);
}

#[test]
fn orchestrator_read_commands_default_to_text_and_one_dlq_mode() {
    let cli = Cli::parse_from(["harn", "orchestrator", "inspect"]);
    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Inspect(inspect) = args.command else {
        panic!("expected orchestrator inspect");
    };
    assert!(!inspect.json);

    let cli = Cli::parse_from(["harn", "orchestrator", "replay", "trigger_evt_123"]);
    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Replay(replay) = args.command else {
        panic!("expected orchestrator replay");
    };
    assert!(!replay.json);

    let cli = Cli::parse_from(["harn", "orchestrator", "dlq", "--replay", "dlq_123"]);
    let Command::Orchestrator(args) = cli.command.unwrap() else {
        panic!("expected orchestrator command");
    };
    let OrchestratorCommand::Dlq(dlq) = args.command else {
        panic!("expected orchestrator dlq");
    };
    assert!(dlq.discard.is_none());
    assert!(!dlq.list);
    assert!(!dlq.json);
}

#[test]
fn bare_session_list_uses_the_current_workspace_and_a_nonzero_limit() {
    let cli = Cli::parse_from(["harn", "session", "list"]);
    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::List(list) = args.command else {
        panic!("expected session list");
    };
    assert_eq!(list.session_root, None);
    assert!(list.limit > 0);
}
