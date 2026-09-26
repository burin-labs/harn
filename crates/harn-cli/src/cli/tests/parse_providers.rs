use super::*;

#[test]
fn connect_preserves_unknown_provider_arguments() {
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
        [
            "acme",
            "--client-id",
            "acme-client",
            "--scope",
            "tickets.read",
            "--no-open"
        ]
    );
}

#[test]
fn models_lora_train_defaults_to_external_trainer_contract() {
    let cli = Cli::parse_from([
        "harn",
        "models",
        "lora",
        "train",
        "--base",
        "local-gemma4-e4b",
        "--dataset",
        "tool-calls.jsonl",
        "--output-dir",
        "adapter",
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
    assert_eq!(args.backend_recipe, "explicit_argv");
    assert!(args.backend_runner.is_empty());
    assert!(args.backend_script.is_none());
    assert!(args.backend_config.is_none());
}

#[test]
fn models_lora_plan_defaults_to_external_trainer_contract() {
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
fn provider_overlay_audit_requires_an_overlay() {
    assert!(Cli::try_parse_from(["harn", "provider", "catalog", "overlay-audit"]).is_err());
}

#[test]
fn provider_probe_defaults_to_json() {
    let cli = Cli::parse_from(["harn", "provider", "probe", "ollama"]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::Probe(args) = provider.command else {
        panic!("expected provider probe command");
    };
    assert!(args.json);
}

#[test]
fn provider_tool_scorecard_modes_have_distinct_defaults() {
    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-scorecard",
        "--tool-probe-report",
        "anthropic.json",
        "--json=false",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolScorecard(report) = provider.command else {
        panic!("expected provider tool-scorecard command");
    };
    assert!(!report.plan_from_catalog);
    assert!(report.routes.is_empty());
    assert!(!report.include_batch_manifest);
    assert!(!report.markdown);
    assert!(!report.json);

    let cli = Cli::parse_from([
        "harn",
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
    ]);
    let Command::Provider(provider) = cli.command.unwrap() else {
        panic!("expected provider command");
    };
    let ProviderCommand::ToolScorecard(plan) = provider.command else {
        panic!("expected provider tool-scorecard command");
    };
    assert!(plan.plan_from_catalog);
    assert!(plan.tool_probe_reports.is_empty());
    assert!(plan.json);
}

#[test]
fn provider_model_completion_candidates_stay_permissive() {
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
    for id in ["provider", "model"] {
        assert!(
            !provider_ready
                .get_arguments()
                .find(|arg| arg.get_id() == id)
                .expect("completion-backed argument")
                .get_possible_values()
                .is_empty(),
            "{id} should still offer completion candidates"
        );
    }
}

/// Every canonical portable option id is accepted by `--option`.
///
/// The vocabulary is declared once and spells these ids with underscores, and
/// the capability field is built from the same spelling. Deriving `ValueEnum`
/// gave the flag a second, kebab-case spelling, so the four ids carrying an
/// underscore were refused before any request could be made. `temperature`,
/// `seed` and `stop` are single words and never showed the defect, which is why
/// this asserts over the whole vocabulary rather than the four that broke.
#[test]
fn option_probe_accepts_every_canonical_option_id() {
    for option in ProviderPortableOptionArg::ALL {
        let canonical = option.name();
        let cli = Cli::parse_from([
            "harn",
            "provider",
            "option-probe",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--option",
            canonical,
        ]);
        let Command::Provider(provider) = cli.command.unwrap() else {
            panic!("expected provider command for {canonical}");
        };
        let ProviderCommand::OptionProbe(args) = provider.command else {
            panic!("expected provider option-probe command for {canonical}");
        };
        assert_eq!(
            args.option, option,
            "--option {canonical} should parse as the variant it names",
        );
    }
}

/// The kebab-case spelling the derive used to produce keeps working.
#[test]
fn option_probe_still_accepts_the_hyphenated_spelling() {
    for (spelled, expected) in [
        ("top-p", ProviderPortableOptionArg::TopP),
        ("top-k", ProviderPortableOptionArg::TopK),
        (
            "frequency-penalty",
            ProviderPortableOptionArg::FrequencyPenalty,
        ),
        (
            "presence-penalty",
            ProviderPortableOptionArg::PresencePenalty,
        ),
    ] {
        let cli = Cli::parse_from([
            "harn",
            "provider",
            "option-probe",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--option",
            spelled,
        ]);
        let Command::Provider(provider) = cli.command.unwrap() else {
            panic!("expected provider command for {spelled}");
        };
        let ProviderCommand::OptionProbe(args) = provider.command else {
            panic!("expected provider option-probe command for {spelled}");
        };
        assert_eq!(args.option, expected, "{spelled} should remain an alias");
    }
}

/// The control. Widening the accepted spellings must not accept anything else.
///
/// Without this, an implementation that took any string would pass both tests
/// above.
#[test]
fn option_probe_still_refuses_an_unknown_option_id() {
    for unknown in ["presence_penalties", "top_n", "", "penalty"] {
        let parsed = Cli::try_parse_from([
            "harn",
            "provider",
            "option-probe",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--option",
            unknown,
        ]);
        let error = parsed.expect_err(&format!("--option {unknown:?} should be refused"));
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::InvalidValue,
            "--option {unknown:?} should be refused as an invalid value",
        );
    }
}
