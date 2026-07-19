use super::*;
use crate::cli::PersonaTemplateKind;

#[test]
fn test_parses_host_lease_acquire_contract() {
    let cli = Cli::parse_from([
        "harn",
        "host",
        "lease",
        "acquire",
        "--host",
        "build-01",
        "--owner",
        "eval-runner",
        "--resource-class",
        "rust-heavy",
        "--priority-class",
        "measurement",
        "--no-expiry",
        "--owner-pid",
        "42",
        "--wait-ms",
        "30000",
        "--json",
    ]);

    let Command::Host(args) = cli.command.unwrap() else {
        panic!("expected host command");
    };
    let HostCommand::Lease(lease) = args.command;
    let HostLeaseCommand::Acquire(acquire) = lease.command else {
        panic!("expected host lease acquire command");
    };
    assert_eq!(acquire.host.as_deref(), Some("build-01"));
    assert_eq!(acquire.owner, "eval-runner");
    assert!(matches!(
        acquire.priority_class,
        HostLeasePriorityArg::Measurement
    ));
    assert!(matches!(
        acquire.resource_class,
        HostLeaseResourceClassArg::RustHeavy
    ));
    assert!(acquire.no_expiry);
    assert_eq!(acquire.owner_pid, Some(42));
    assert_eq!(acquire.wait_ms, 30_000);
    assert!(acquire.json);
}

#[test]
fn test_parses_supervised_cargo_lease_contract() {
    let cli = Cli::parse_from([
        "harn",
        "host",
        "lease",
        "run",
        "cargo",
        "--owner",
        "codex-0",
        "--host",
        "build-01",
        "--wait-ms",
        "30000",
        "--workspace",
        "/workspace",
        "--target-dir",
        "/target",
        "--build-dir",
        "/build",
        "--",
        "check",
        "-p",
        "harn-cli",
    ]);

    let Command::Host(args) = cli.command.unwrap() else {
        panic!("expected host command");
    };
    let HostCommand::Lease(lease) = args.command;
    let HostLeaseCommand::Run(run) = lease.command else {
        panic!("expected host lease run command");
    };
    let HostLeaseRunCommand::Cargo(cargo) = run.command;
    assert_eq!(cargo.owner, "codex-0");
    assert_eq!(cargo.host.as_deref(), Some("build-01"));
    assert!(matches!(
        cargo.priority_class,
        HostLeasePriorityArg::CiVerify
    ));
    assert_eq!(cargo.wait_ms, 30_000);
    assert_eq!(cargo.workspace, PathBuf::from("/workspace"));
    assert_eq!(cargo.target_dir, PathBuf::from("/target"));
    assert_eq!(cargo.build_dir, Some(PathBuf::from("/build")));
    assert_eq!(
        cargo.cargo_args,
        vec![
            "check".to_string(),
            "-p".to_string(),
            "harn-cli".to_string()
        ]
    );
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
fn test_parses_canon_check_args() {
    let cli = Cli::parse_from([
        "harn",
        "canon",
        "check",
        "src/main.zig",
        "--canon-root",
        "vendor/harn-canon",
        "--workspace-root",
        "workspace",
        "--pack",
        "zig",
        "--include-semantic",
        "--budget-ms",
        "75",
        "--advisory",
        "--json",
    ]);

    let Command::Canon(args) = cli.command.unwrap() else {
        panic!("expected canon command");
    };
    match args.command {
        CanonCommand::Check(check) => {
            assert_eq!(check.paths, vec![PathBuf::from("src/main.zig")]);
            assert_eq!(check.canon_root, Some(PathBuf::from("vendor/harn-canon")));
            assert_eq!(check.workspace_root, PathBuf::from("workspace"));
            assert_eq!(check.packs, vec!["zig"]);
            assert!(check.include_semantic);
            assert_eq!(check.budget_ms, 75);
            assert!(check.advisory);
            assert!(check.json);
        }
    }
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
    let RunsCommand::Inspect(inspect) = args.command else {
        panic!("expected runs inspect command");
    };
    assert_eq!(inspect.path, "run.json");
    assert_eq!(inspect.compare.as_deref(), Some("baseline.json"));
}

#[test]
fn test_parses_runs_view_json_session() {
    let cli = Cli::parse_from(["harn", "runs", "view", "runs", "--session", "--json"]);

    let Command::Runs(args) = cli.command.unwrap() else {
        panic!("expected runs command");
    };
    let RunsCommand::View(view) = args.command else {
        panic!("expected runs view command");
    };
    assert_eq!(view.path, "runs");
    assert!(view.session);
    assert!(view.json);
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
        "checkpoint",
        "worker.json",
        "--out",
        "checkpoint.bundle.json",
        "--sanitized",
    ]);

    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Checkpoint(checkpoint) = args.command else {
        panic!("expected session checkpoint");
    };
    assert_eq!(checkpoint.worker_snapshot, "worker.json");
    assert_eq!(checkpoint.out.as_deref(), Some("checkpoint.bundle.json"));
    assert!(checkpoint.sanitized);

    let cli = Cli::parse_from([
        "harn",
        "session",
        "import",
        "bundle.json",
        "--out",
        "imported.json",
        "--worker-snapshot-dir",
        "workers",
        "--allow-unsafe-secret-markers",
        "--json",
    ]);

    let Command::Session(args) = cli.command.unwrap() else {
        panic!("expected session command");
    };
    let SessionCommand::Import(import) = args.command else {
        panic!("expected session import");
    };
    assert_eq!(import.bundle, "bundle.json");
    assert_eq!(import.out.as_deref(), Some("imported.json"));
    assert_eq!(import.worker_snapshot_dir.as_deref(), Some("workers"));
    assert!(import.allow_unsafe_secret_markers);
    assert!(import.json);

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
    let MergeCaptainCommand::Ladder(ladder) = args.command else {
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
    let MergeCaptainCommand::Ladder(ladder) = args.command else {
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
    let MergeCaptainCommand::Iterate(iterate) = args.command else {
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
    let MergeCaptainCommand::Iterate(iterate) = args.command else {
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
fn test_parses_manual_persona_new_flags() {
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
    let PersonaCommand::New(new) = args.command else {
        panic!("expected persona new command");
    };
    assert_eq!(new.name.as_deref(), Some("incident_triager"));
    assert_eq!(
        new.template,
        Some(PersonaTemplateKind::HybridClassifyThenAct)
    );
    assert!(new.from_prompt.is_none());
}

#[test]
fn test_parses_persona_compile_prompt_flags() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "compile-prompt",
        "--prompt",
        "Triage Slack alerts.",
        "--name",
        "alerts_triage",
        "--provider",
        "mock",
        "--model",
        "mock-model",
        "--max-tokens",
        "1200",
        "--json",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::CompilePrompt(compile) = args.command else {
        panic!("expected persona compile-prompt command");
    };
    assert_eq!(compile.prompt, "Triage Slack alerts.");
    assert_eq!(compile.name.as_deref(), Some("alerts_triage"));
    assert_eq!(compile.provider.as_deref(), Some("mock"));
    assert_eq!(compile.model.as_deref(), Some("mock-model"));
    assert_eq!(compile.max_tokens, 1200);
    assert!(compile.json);
}

#[test]
fn test_parses_persona_new_from_prompt_flags() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "new",
        "--from-prompt",
        "Digest replies every four hours.",
        "--name",
        "reply_digest",
        "--provider",
        "mock",
        "--max-tokens",
        "512",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::New(new) = args.command else {
        panic!("expected persona new command");
    };
    assert!(new.name.is_none());
    assert!(new.template.is_none());
    assert_eq!(
        new.from_prompt.as_deref(),
        Some("Digest replies every four hours.")
    );
    assert_eq!(new.prompt_name.as_deref(), Some("reply_digest"));
    assert_eq!(new.provider.as_deref(), Some("mock"));
    assert_eq!(new.max_tokens, Some(512));
}

#[test]
fn test_persona_prompt_token_ceiling_is_hard() {
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
fn test_parses_persona_materialize_flags() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "materialize",
        "--blueprint",
        "persona.blueprint.json",
        "--output-root",
        "generated-personas",
        "--force",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::Materialize(materialize) = args.command else {
        panic!("expected persona materialize command");
    };
    assert_eq!(
        materialize.blueprint,
        Some(PathBuf::from("persona.blueprint.json"))
    );
    assert!(materialize.compile_receipt.is_none());
    assert_eq!(materialize.output_root, PathBuf::from("generated-personas"));
    assert!(materialize.force);
    assert!(!materialize.activate);
    assert!(!materialize.json);
}

#[test]
fn test_parses_persona_materialize_compile_receipt() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "materialize",
        "--compile-receipt",
        "reviewed-receipt.json",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::Materialize(materialize) = args.command else {
        panic!("expected persona materialize command");
    };
    assert!(materialize.blueprint.is_none());
    assert_eq!(
        materialize.compile_receipt,
        Some(PathBuf::from("reviewed-receipt.json"))
    );
    assert!(!materialize.activate);
    assert!(!materialize.json);
}

#[test]
fn test_parses_persona_materialize_apply() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "materialize",
        "--compile-receipt",
        "reviewed-receipt.json",
        "--manifest",
        "project/harn.toml",
        "--activate",
        "--json",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    assert_eq!(args.manifest, Some(PathBuf::from("project/harn.toml")));
    let PersonaCommand::Materialize(materialize) = args.command else {
        panic!("expected persona materialize command");
    };
    assert!(materialize.activate);
    assert!(materialize.json);
}

#[test]
fn test_persona_materialize_apply_requires_manifest_and_json() {
    for (missing, args) in [
        (
            "--manifest",
            vec![
                "harn",
                "persona",
                "materialize",
                "--compile-receipt",
                "reviewed-receipt.json",
                "--activate",
                "--json",
            ],
        ),
        (
            "--json",
            vec![
                "harn",
                "persona",
                "materialize",
                "--compile-receipt",
                "reviewed-receipt.json",
                "--manifest",
                "project/harn.toml",
                "--activate",
            ],
        ),
    ] {
        let error = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "missing {missing} must fail during argument parsing"
        );
        assert!(error.to_string().contains(missing));
    }
}

#[test]
fn test_persona_materialize_requires_exactly_one_input() {
    let neither = Cli::try_parse_from(["harn", "persona", "materialize"]).unwrap_err();
    assert_eq!(
        neither.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );

    let both = Cli::try_parse_from([
        "harn",
        "persona",
        "materialize",
        "--blueprint",
        "blueprint.json",
        "--compile-receipt",
        "receipt.json",
    ])
    .unwrap_err();
    assert_eq!(both.kind(), clap::error::ErrorKind::ArgumentConflict);

    let blueprint_apply = Cli::try_parse_from([
        "harn",
        "persona",
        "materialize",
        "--blueprint",
        "blueprint.json",
        "--activate",
    ])
    .unwrap_err();
    assert_eq!(
        blueprint_apply.kind(),
        clap::error::ErrorKind::ArgumentConflict
    );

    let json_without_apply = Cli::try_parse_from([
        "harn",
        "persona",
        "materialize",
        "--compile-receipt",
        "receipt.json",
        "--json",
    ])
    .unwrap_err();
    assert_eq!(
        json_without_apply.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn test_parses_persona_activation_attenuation() {
    let cli = Cli::parse_from([
        "harn",
        "persona",
        "--manifest",
        "workspace/harn.toml",
        "activate",
        "agents/reviewer",
        "--autonomy-tier",
        "suggest",
        "--tool",
        "filesystem",
        "--capability",
        "workspace.read_text",
        "--json",
    ]);

    let Command::Persona(args) = cli.command.unwrap() else {
        panic!("expected persona command");
    };
    let PersonaCommand::Activate(activate) = args.command else {
        panic!("expected persona activate command");
    };
    assert_eq!(activate.name, "agents/reviewer");
    assert_eq!(activate.autonomy_tier.as_deref(), Some("suggest"));
    assert_eq!(activate.tools, vec!["filesystem"]);
    assert_eq!(activate.capabilities, vec!["workspace.read_text"]);
    assert!(activate.json);
}

#[test]
fn test_persona_activation_rejects_unenforced_budget_flags() {
    let error = Cli::try_parse_from([
        "harn",
        "persona",
        "activate",
        "agents/reviewer",
        "--daily-usd",
        "5",
    ])
    .unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn test_persona_activation_rejects_conflicting_set_flags() {
    let error = Cli::try_parse_from([
        "harn",
        "persona",
        "activate",
        "agents/reviewer",
        "--tool",
        "filesystem",
        "--no-tools",
    ])
    .unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
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
