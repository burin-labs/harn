//! The CLI dispatch entry point: the async `clap` command match that every
//! `harn` invocation lands in, plus the argument shims and one-off subcommand
//! runners it owns.

use crate::*;

#[allow(clippy::large_stack_frames)] // dispatch entrypoint owns full Args + per-feature locals.
pub(crate) async fn async_main(raw_args: Vec<String>, runtime_mode: CliRuntimeMode) {
    // Install the OTLP exporter sink before any subcommand runs so a
    // 20+ minute autonomous session has spans streaming to the
    // configured collector from the first turn. When neither
    // `HARN_OTEL_ENDPOINT` nor `OTEL_EXPORTER_OTLP_ENDPOINT` is set
    // this is a no-op. A misconfigured endpoint logs and continues —
    // local observability is opt-in and must never fail the run.
    if runtime_mode.enables_tokio_io() {
        if let Err(error) = harn_vm::events::install_otel_sink_from_env() {
            eprintln!("[harn] OTel exporter disabled: {error}");
        }
    }

    if raw_args.len() == 2 && raw_args[1].ends_with(".harn") {
        provider_bootstrap::maybe_seed_ollama_for_run_file(Path::new(&raw_args[1]), false, false)
            .await;
        commands::run::run_file(
            &raw_args[1],
            false,
            std::collections::HashSet::new(),
            Vec::new(),
            commands::run::CliLlmMockMode::Off,
            None,
            commands::run::RunProfileOptions::default(),
        )
        .await;
        return;
    }

    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                error.exit();
            }
            error.exit();
        }
    };

    if cli.json_schemas {
        commands::json_schemas::run(cli.schema_command.as_deref());
        return;
    }

    let Some(subcommand) = cli.command else {
        // `arg_required_else_help` already shows help when no args are
        // supplied. We only land here if a top-level flag (e.g. a
        // future `--version` long flag) parsed without a subcommand.
        let mut cmd = Cli::command();
        cmd.print_help().ok();
        return;
    };
    match subcommand {
        Command::Version(args) => {
            let exit = run_version(args).await;
            if exit != 0 {
                process::exit(exit);
            }
        }
        Command::Upgrade(args) => {
            if let Err(error) = commands::upgrade::run(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Dap(_) => run_dap_adapter(),
        Command::ConformanceHelper(args) => {
            if let Err(error) = commands::conformance_helper::run(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Skill(args) => match args.command {
            SkillCommand::List(list) => commands::skills::run_list(&list),
            SkillCommand::Get(get) => commands::skills::run_get(&get),
            SkillCommand::Dump(dump) => commands::skills::run_dump(&dump),
            SkillCommand::Resolved(resolved) => commands::skills::run_resolved(&resolved),
            SkillCommand::Inspect(inspect) => commands::skills::run_inspect(&inspect),
            SkillCommand::Match(matcher) => commands::skills::run_match(&matcher),
            SkillCommand::Install(install) => commands::skills::run_install(&install),
            SkillCommand::New(new_args) => commands::skills::run_new(&new_args),
            SkillCommand::Key(key_args) => match key_args.command {
                SkillKeyCommand::Generate(generate) => commands::skill::run_key_generate(&generate),
            },
            SkillCommand::Sign(sign) => commands::skill::run_sign(&sign),
            SkillCommand::Endorse(endorse) => commands::skill::run_endorse(&endorse),
            SkillCommand::Verify(verify) => commands::skill::run_verify(&verify),
            SkillCommand::WhoSigned(who_signed) => {
                commands::skill::run_who_signed(&who_signed).await;
            }
            SkillCommand::Trust(trust_args) => match trust_args.command {
                SkillTrustCommand::Add(add) => commands::skill::run_trust_add(&add),
                SkillTrustCommand::List(list) => commands::skill::run_trust_list(&list),
            },
        },
        Command::Guard(args) => match args.command {
            GuardCommand::List(list_args) => commands::guard::run_list(&list_args),
            GuardCommand::Install(install_args) => {
                commands::guard::run_install(&install_args).await;
            }
            GuardCommand::Status(status_args) => commands::guard::run_status(&status_args),
            GuardCommand::Remove(remove_args) => commands::guard::run_remove(&remove_args),
        },
        Command::Run(args) => {
            let _operator_approval_guard = args.install_operator_approval_grant();
            if !args.explain_cost {
                match (args.eval.as_deref(), args.file.as_deref()) {
                    (Some(code), None) => {
                        provider_bootstrap::maybe_seed_ollama_for_inline(
                            code,
                            args.yes,
                            args.llm_mock.is_some(),
                        )
                        .await;
                    }
                    (None, Some(file)) => {
                        provider_bootstrap::maybe_seed_ollama_for_run_file(
                            Path::new(file),
                            args.yes,
                            args.llm_mock.is_some(),
                        )
                        .await;
                    }
                    _ => {}
                }
            }
            let denied =
                commands::run::build_denied_builtins(args.deny.as_deref(), args.allow.as_deref());
            let llm_mock_mode = if let Some(path) = args.llm_mock.as_ref() {
                commands::run::CliLlmMockMode::Replay {
                    fixture_path: PathBuf::from(path),
                }
            } else if let Some(path) = args.llm_mock_record.as_ref() {
                commands::run::CliLlmMockMode::Record {
                    fixture_path: PathBuf::from(path),
                }
            } else {
                commands::run::CliLlmMockMode::Off
            };
            let attestation = args.attest.then(|| commands::run::RunAttestationOptions {
                receipt_out: args.receipt_out.as_ref().map(PathBuf::from),
                agent_id: args.attest_agent.clone(),
            });
            let profile_options = run_profile_options(&args.profile);
            let sandbox_options = commands::run::sandbox::sandbox_options_from_args(&args.sandbox);
            let json_options = args
                .json
                .then_some(commands::run::RunJsonOptions { quiet: args.quiet });
            let aux_options = commands::run::run_aux_options_from_args(&args);
            let control_options = commands::run::run_control_options_from_args(&args);
            let harnpack_options = commands::run::harnpack::HarnpackRunOptions {
                allow_unsigned: args.allow_unsigned,
                dry_run_verify: args.dry_run_verify,
            };
            if let Some(resume_target) = args.resume.as_deref() {
                let exit_code = commands::run::run_resume_with_skill_dirs(
                    resume_target,
                    args.trace,
                    denied,
                    args.argv.clone(),
                    args.skill_dir.clone(),
                    llm_mock_mode,
                    attestation,
                    profile_options,
                    sandbox_options.clone(),
                    json_options,
                    aux_options,
                    control_options,
                )
                .await;
                runtime::exit_on_error(exit_code);
                return;
            }

            if args.as_job {
                run_as_job(&args).await;
                return;
            }

            match (args.eval.as_deref(), args.file.as_deref()) {
                (Some(code), None) => {
                    if args.allow_unsigned || args.dry_run_verify {
                        command_error(
                            "`--allow-unsigned` and `--dry-run-verify` apply to `.harnpack` inputs; \
                             they cannot be combined with `-e`",
                        );
                    }
                    let (wrapped, tmp) = commands::run::prepare_eval_temp_file(code)
                        .unwrap_or_else(|e| command_error(&e));
                    let tmp_path: PathBuf = tmp.path().to_path_buf();
                    if let Err(error) = fs::write(&tmp_path, &wrapped) {
                        drop(tmp);
                        command_error(&format!("failed to write temp file for -e: {error}"));
                    }
                    let tmp_str = tmp_path.to_string_lossy().into_owned();
                    let exit_code = if args.explain_cost {
                        commands::run::run_explain_cost_file_with_skill_dirs(&tmp_str)
                    } else {
                        commands::run::run_file_with_skill_dirs(
                            &tmp_str,
                            args.trace,
                            denied,
                            args.argv.clone(),
                            args.skill_dir.clone(),
                            llm_mock_mode.clone(),
                            attestation.clone(),
                            profile_options.clone(),
                            sandbox_options.clone(),
                            json_options.clone(),
                            aux_options.clone(),
                            control_options.clone(),
                            harnpack_options.clone(),
                        )
                        .await
                    };
                    drop(tmp);
                    runtime::exit_on_error(exit_code);
                }
                (None, Some(file)) => {
                    let exit_code = if args.explain_cost {
                        commands::run::run_explain_cost_file_with_skill_dirs(file)
                    } else {
                        commands::run::run_file_with_skill_dirs(
                            file,
                            args.trace,
                            denied,
                            args.argv.clone(),
                            args.skill_dir.clone(),
                            llm_mock_mode,
                            attestation,
                            profile_options,
                            sandbox_options,
                            json_options,
                            aux_options,
                            control_options,
                            harnpack_options,
                        )
                        .await
                    };
                    runtime::exit_on_error(exit_code);
                }
                (Some(_), Some(_)) => command_error(
                    "`harn run` accepts either `-e <code>` or `<file.harn>`, not both",
                ),
                (None, None) => command_error(
                    "`harn run` requires `--resume <snapshot>`, `-e <code>`, or `<file.harn>`",
                ),
            }
        }
        Command::Check(args) => commands::check::run_check_command(args),
        Command::Parse(args) => {
            if let Err(error) = commands::parse_tokens::run_parse(&args) {
                command_error(&error);
            }
        }
        Command::Tokens(args) => {
            if let Err(error) = commands::parse_tokens::run_tokens(&args) {
                command_error(&error);
            }
        }
        Command::Config(args) => {
            if let Err(error) = commands::config_cmd::run(args).await {
                command_error(&error);
            }
        }
        Command::Explain(args) => {
            let code = commands::explain::run_explain(&args).await;
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Fix(args) => {
            if let Err(error) = commands::fix::run(&args) {
                if error.is_partial_failure() {
                    eprintln!("error: {}", error.message());
                    process::exit(1);
                }
                command_error(error.message());
            }
        }
        Command::Contracts(args) => {
            commands::contracts::handle_contracts_command(args).await;
        }
        Command::Connect(args) => {
            commands::connect::run_connect(*args).await;
        }
        Command::Lint(args) => {
            if commands::check::run_changed_lint_command(&args).await {
                return;
            }
            let targets: Vec<&str> = args.targets.iter().map(String::as_str).collect();
            let (files, prompt_files) = commands::check::collect_lint_targets(&targets);
            if files.is_empty() && prompt_files.is_empty() {
                if args.json {
                    print_lint_error(
                        "no_lint_targets",
                        "no .harn, .harn.txt, or .harn.prompt files found under the given target(s)",
                    );
                }
                command_error(
                    "no .harn, .harn.txt, or .harn.prompt files found under the given target(s)",
                );
            }
            if args.json {
                let outcome = commands::check::run_lint_json(
                    &files,
                    commands::check::LintJsonOptions {
                        strict: args.strict,
                        require_file_header: args.require_file_header,
                        require_public_api_types: args.require_public_api_types,
                    },
                )
                .await;
                println!("{}", json_envelope::to_string_pretty(&outcome.envelope));
                runtime::exit_on_error(outcome.exit_code);
                return;
            }
            let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
            let module_graph =
                commands::check::build_module_graph_and_seed_analysis(&files, &mut analysis);
            let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
            // Run project script rules once, then merge their diagnostics per file.
            let script_rule_diags = commands::check::run_project_script_rules(&files).await;
            let script_diags_for = |file: &std::path::Path| -> &[harn_lint::LintDiagnostic] {
                script_rule_diags
                    .get(file)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            };
            if args.fix {
                let mut should_fail = false;
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    let mut lint_config = commands::check::load_harn_lint_config(file);
                    lint_config.require_file_header |= args.require_file_header;
                    lint_config.require_public_api_types |= args.require_public_api_types;
                    commands::check::apply_loaded_harn_lint_config(&lint_config, &mut config);
                    let outcome = commands::check::lint_fix_file(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        &lint_config,
                    );
                    should_fail |= outcome.should_fail(config.strict || args.strict);
                }
                for file in &prompt_files {
                    let lint_config = commands::check::load_harn_lint_config(file);
                    let config = package::load_check_config(Some(file));
                    // Template lint rules carry no autofix edits yet.
                    let outcome = commands::check::lint_prompt_file_inner(
                        file,
                        lint_config.template_variant_branch_threshold,
                        &lint_config.disabled,
                    );
                    should_fail |= outcome.should_fail(config.strict || args.strict);
                }
                // Autofix does not suppress residual failures: unfixable
                // error-level diagnostics (and warnings under `--strict`) must
                // still fail the exit code exactly like the plain lint path, so
                // CI/pre-commit hooks running `--fix` never pass green over a
                // real error.
                if should_fail {
                    process::exit(1);
                }
            } else {
                let mut should_fail = false;
                let mut total_findings = 0usize;
                let mut total_fixable = 0usize;
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    let mut lint_config = commands::check::load_harn_lint_config(file);
                    lint_config.require_file_header |= args.require_file_header;
                    lint_config.require_public_api_types |= args.require_public_api_types;
                    commands::check::apply_loaded_harn_lint_config(&lint_config, &mut config);
                    let outcome = commands::check::lint_file_inner(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        &lint_config,
                        script_diags_for(file.as_path()),
                    );
                    total_findings += outcome.findings;
                    total_fixable += outcome.fixable;
                    should_fail |= outcome.should_fail(config.strict || args.strict);
                }
                for file in &prompt_files {
                    let lint_config = commands::check::load_harn_lint_config(file);
                    let config = package::load_check_config(Some(file));
                    let outcome = commands::check::lint_prompt_file_inner(
                        file,
                        lint_config.template_variant_branch_threshold,
                        &lint_config.disabled,
                    );
                    total_findings += outcome.findings;
                    total_fixable += outcome.fixable;
                    should_fail |= outcome.should_fail(config.strict || args.strict);
                }
                // ESLint-style hint: when findings are auto-fixable, point the
                // user at `--fix`. Emphasized when *every* finding is fixable.
                if total_fixable > 0 {
                    if total_fixable == total_findings {
                        eprintln!(
                            "\nAll {total_fixable} finding(s) are auto-fixable — run `harn lint --fix` to apply them."
                        );
                    } else {
                        eprintln!(
                            "\n{total_fixable} of {total_findings} finding(s) are auto-fixable — run `harn lint --fix` to apply them."
                        );
                    }
                }
                if should_fail {
                    process::exit(1);
                }
            }
        }
        Command::Fmt(args) => {
            let targets: Vec<&str> = args.targets.iter().map(String::as_str).collect();
            // Anchor config resolution on the first target; CLI flags
            // always win over harn.toml values.
            let anchor = targets.first().map(Path::new).unwrap_or(Path::new("."));
            let loaded = match project_config::load_for_path(anchor) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: {e}");
                    project_config::HarnConfig::default()
                }
            };
            let mut opts = harn_fmt::FmtOptions::default();
            if let Some(w) = loaded.fmt.line_width {
                opts.line_width = w;
            }
            if let Some(w) = loaded.fmt.separator_width {
                opts.separator_width = w;
            }
            if let Some(w) = args.line_width {
                opts.line_width = w;
            }
            if let Some(w) = args.separator_width {
                opts.separator_width = w;
            }
            let mode = commands::check::FmtMode::from_check_flag(args.check);
            if args.json {
                let envelope = commands::check::fmt_targets_json(&targets, mode, &opts);
                let failed = !envelope.ok;
                println!("{}", json_envelope::to_string_pretty(&envelope));
                if failed {
                    process::exit(1);
                }
            } else {
                commands::check::fmt_targets(&targets, mode, &opts);
            }
        }
        Command::Test(args) => commands::test::run_command(args).await,
        Command::Init(args) => {
            commands::init::init_project(args.name.as_deref(), args.template).await;
        }
        Command::New(args) => match commands::init::resolve_new_args(&args) {
            Ok((name, template)) => commands::init::init_project(name.as_deref(), template).await,
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        },
        Command::Doctor(args) => {
            commands::doctor::run_doctor_with_options(commands::doctor::DoctorOptions {
                json: args.json,
                check_providers: args.check_providers,
                check_targets: args.check_targets,
            })
            .await;
        }
        Command::Host(args) => {
            let exit = commands::host::run(args).await;
            if exit != 0 {
                process::exit(exit);
            }
        }
        Command::Models(args) => commands::models::run(args).await,
        Command::Local(args) => commands::local::run(args).await,
        Command::Provider(args) => match args.command {
            ProviderCommand::Capabilities(capabilities) => {
                commands::provider_capabilities::run_or_exit(capabilities);
            }
            ProviderCommand::Catalog(catalog) => {
                commands::providers::dispatch_catalog(catalog.command).await;
            }
            ProviderCommand::Ready(ready) => {
                run_provider_ready(
                    &ready.provider,
                    ready.model.as_deref(),
                    ready.base_url.as_deref(),
                    ready.json,
                )
                .await;
            }
            ProviderCommand::Probe(probe) => commands::provider::run_provider_probe(probe).await,
            ProviderCommand::ToolProbe(args) => commands::provider::run_tool_probe(args).await,
            ProviderCommand::ToolCalibrate(args) => commands::run_tool_calibrate(args).await,
            ProviderCommand::ToolProbeAudit(args) => commands::providers::run_audit(args),
            ProviderCommand::ToolScorecard(tool_scorecard) => {
                commands::provider::run_provider_tool_scorecard(tool_scorecard).await;
            }
            ProviderCommand::CacheProbe(cache_probe) => {
                commands::provider::run_provider_cache_probe(cache_probe).await;
            }
            ProviderCommand::DispatchExplain(explain) => commands::dispatch_explain::run(&explain),
            ProviderCommand::DispatchAudit(audit) => commands::dispatch_explain::run_audit(&audit),
            ProviderCommand::Limits(limits) => {
                commands::provider_limits::run(&limits);
            }
        },
        Command::Scan(args) => commands::scan::run(args).await,
        Command::Codemod(args) => commands::codemod::run(args).await,
        Command::Rule(args) => commands::rule::run(args).await,
        Command::Try(args) => commands::try_cmd::run(args).await,
        Command::Quickstart(args) => {
            if let Err(error) = commands::quickstart::run_quickstart(&args).await {
                command_error(&error);
            }
        }
        Command::Demo(args) => {
            let code = commands::demo::run(args).await;
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Serve(args) => commands::serve::run_command(args.command).await,
        Command::Connector(args) => {
            if let Err(error) = commands::connector::handle_connector_command(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Mcp(args) => commands::mcp::handle_mcp_command(&args.command).await,
        Command::Watch(args) => {
            let denied =
                commands::run::build_denied_builtins(args.deny.as_deref(), args.allow.as_deref());
            commands::run::run_watch(&args.file, denied).await;
        }
        Command::Dev(args) => {
            commands::dev::run(args).await;
        }
        Command::Portal(args) => {
            commands::portal::run_portal(
                &args.dir,
                args.manifest,
                args.persona_state_dir,
                &args.host,
                args.port,
                args.open,
                args.allow_remote_launch,
            )
            .await;
        }
        Command::Trigger(args) => {
            if let Err(error) = commands::trigger::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Graph(args) => {
            let code = commands::graph::run(args).await;
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Doc(args) => {
            let code = commands::doc::run(&args.path, args.output.as_deref());
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Routes(args) => {
            let code = commands::routes::run(args).await;
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Usage(args) => {
            let code = commands::usage::run(args).await;
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Flow(args) => match commands::flow::run_flow(&args) {
            Ok(code) => {
                if code != 0 {
                    process::exit(code);
                }
            }
            Err(error) => command_error(&error),
        },
        Command::Canon(args) => commands::canon::run(args).await,
        Command::Workflow(args) => match commands::workflow::handle(args) {
            Ok(code) => {
                if code != 0 {
                    process::exit(code);
                }
            }
            Err(error) => command_error(&error),
        },
        Command::Supervisor(args) => {
            if let Err(error) = commands::supervisor::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Trace(args) => {
            if let Err(error) = commands::trace::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Crystallize(args) => {
            if let Err(error) = commands::crystallize::run(args) {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Trust(args) => {
            if let Err(error) = commands::trust::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Verify(args) => {
            if let Err(error) = verify_provenance_receipt(&args.receipt, args.json) {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Completion(args) => print_completions(args.shell),
        Command::Orchestrator(args) => {
            if let Err(error) = commands::orchestrator::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Playground(args) => {
            provider_bootstrap::maybe_seed_ollama_for_playground(
                Path::new(&args.host),
                Path::new(&args.script),
                args.yes,
                args.llm.is_some(),
                args.llm_mock.is_some(),
            )
            .await;
            let llm_mock_mode = if let Some(path) = args.llm_mock.as_ref() {
                commands::run::CliLlmMockMode::Replay {
                    fixture_path: PathBuf::from(path),
                }
            } else if let Some(path) = args.llm_mock_record.as_ref() {
                commands::run::CliLlmMockMode::Record {
                    fixture_path: PathBuf::from(path),
                }
            } else {
                commands::run::CliLlmMockMode::Off
            };
            if let Err(error) = commands::playground::run_command(args, llm_mock_mode).await {
                eprint!("{error}");
                process::exit(1);
            }
        }
        Command::Runs(args) => cli::run_runs_command(args).await,
        Command::Session(args) => commands::session::run(args),
        Command::Replay(args) => {
            let exit = commands::replay::run(args);
            if exit != 0 {
                process::exit(exit);
            }
        }
        Command::Eval(args) => match args.command {
            Some(EvalCommand::CodingAgent(coding_agent_args)) => {
                let code = commands::eval_coding_agent::run(coding_agent_args).await;
                if code != 0 {
                    process::exit(code);
                }
            }
            Some(EvalCommand::Context(context_args)) => {
                let code = commands::eval_context::run(context_args).await;
                if code != 0 {
                    process::exit(code);
                }
            }
            Some(EvalCommand::Prompt(prompt_args)) => {
                let code = commands::eval_prompt::run(prompt_args).await;
                if code != 0 {
                    process::exit(code);
                }
            }
            Some(EvalCommand::SkillGate(skill_gate_args)) => {
                let code = commands::eval_skill_gate::run(skill_gate_args).await;
                if code != 0 {
                    process::exit(code);
                }
            }
            Some(EvalCommand::ScopeTriage(scope_args)) => {
                process::exit(commands::eval_scope_triage::run(scope_args).await)
            }
            Some(EvalCommand::ToolCalls(tool_calls_args)) => {
                let code = commands::eval_tool_calls::run(tool_calls_args).await;
                if code != 0 {
                    process::exit(code);
                }
            }
            None => {
                let Some(path) = args.path else {
                    eprintln!("error: `harn eval` requires a path or a subcommand (e.g. `prompt`).\nSee `harn eval --help`.");
                    process::exit(2);
                };
                let llm_mock_mode = if let Some(path) = args.llm_mock.as_ref() {
                    commands::run::CliLlmMockMode::Replay {
                        fixture_path: PathBuf::from(path),
                    }
                } else if let Some(path) = args.llm_mock_record.as_ref() {
                    commands::run::CliLlmMockMode::Record {
                        fixture_path: PathBuf::from(path),
                    }
                } else {
                    commands::run::CliLlmMockMode::Off
                };
                eval_run_record(
                    &path,
                    args.compare.as_deref(),
                    args.structural_experiment.as_deref(),
                    &args.argv,
                    &llm_mock_mode,
                );
            }
        },
        Command::Repl => commands::repl::run_repl().await,
        Command::Bench(args) => commands::bench::run(args).await,
        Command::Precompile(args) => commands::precompile::run(args).await,
        Command::Pack(args) => commands::pack::run(args),
        Command::TestBench(args) => commands::test_bench::run(args.command).await,
        Command::Viz(args) => commands::viz::run_viz(&args.file, args.output.as_deref()),
        Command::Install(args) => package::install_packages(
            args.frozen || args.locked || args.offline,
            args.refetch.as_deref(),
            args.offline,
            args.json,
        ),
        Command::Add(args) => package::add_package_with_registry(
            &args.name_or_spec,
            args.alias.as_deref(),
            args.git.as_deref(),
            args.tag.as_deref(),
            args.rev.as_deref(),
            args.branch.as_deref(),
            args.path.as_deref(),
            args.registry.as_deref(),
        ),
        Command::Update(args) => {
            package::update_packages(args.alias.as_deref(), args.all, args.json);
        }
        Command::Remove(args) => package::remove_package(&args.alias),
        Command::Lock => package::lock_packages(),
        Command::Package(args) => match args.command {
            PackageCommand::List(list) => package::list_packages(list.json),
            PackageCommand::Doctor(doctor) => package::doctor_packages(doctor.json),
            PackageCommand::Search(search) => package::search_package_registry(
                search.query.as_deref(),
                search.registry.as_deref(),
                search.json,
            ),
            PackageCommand::Info(info) => {
                package::show_package_registry_info(
                    &info.name,
                    info.registry.as_deref(),
                    info.json,
                );
            }
            PackageCommand::Check(check) => {
                package::check_package(check.package.as_deref(), check.json);
            }
            PackageCommand::Verify(verify) => {
                if let Err(error) = commands::package_verify::handle_package_verify(verify).await {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PackageCommand::Pack(pack) => package::pack_package(
                pack.package.as_deref(),
                pack.output.as_deref(),
                pack.dry_run,
                pack.json,
            ),
            PackageCommand::Docs(docs) => package::generate_package_docs(
                docs.package.as_deref(),
                docs.output.as_deref(),
                docs.check,
            ),
            PackageCommand::Cache(cache) => match cache.command {
                PackageCacheCommand::List => package::list_package_cache(),
                PackageCacheCommand::Clean(clean) => package::clean_package_cache(clean.all),
                PackageCacheCommand::Verify(verify) => {
                    package::verify_package_cache(verify.materialized);
                }
            },
            PackageCommand::Outdated(args) => package::outdated_packages(
                args.refresh,
                args.remote,
                args.registry.as_deref(),
                args.json,
            ),
            PackageCommand::Audit(args) => {
                package::audit_packages(
                    args.registry.as_deref(),
                    args.skip_materialized,
                    args.json,
                );
            }
            PackageCommand::Artifacts(args) => match args.command {
                PackageArtifactsCommand::Manifest(manifest) => {
                    package::artifacts_manifest(manifest.output.as_deref());
                }
                PackageArtifactsCommand::Check(check) => {
                    package::artifacts_check(&check.manifest, check.json);
                }
            },
            PackageCommand::Scaffold(args) => match args.command {
                PackageScaffoldCommand::Openapi(openapi) => {
                    if let Err(error) = commands::package_scaffold::run_openapi(&openapi).await {
                        eprintln!("error: {error}");
                        process::exit(1);
                    }
                }
            },
        },
        Command::Publish(args) => package::publish_package(
            args.package.as_deref(),
            args.dry_run,
            &args.remote,
            &args.index_repo,
            &args.index_path,
            args.registry_name.as_deref(),
            args.skip_index_pr,
            args.registry.as_deref(),
            args.json,
        ),
        Command::MergeCaptain(args) => match args.command {
            MergeCaptainCommand::Run(run) => {
                let code = commands::merge_captain::run_driver(&run);
                if code != 0 {
                    process::exit(code);
                }
            }
            MergeCaptainCommand::Ladder(ladder) => {
                let code = commands::merge_captain::run_ladder(&ladder);
                if code != 0 {
                    process::exit(code);
                }
            }
            MergeCaptainCommand::Iterate(iterate) => {
                let code = commands::merge_captain::run_iterate(&iterate);
                if code != 0 {
                    process::exit(code);
                }
            }
            MergeCaptainCommand::Audit(audit) => {
                let code = commands::merge_captain::run_audit(&audit);
                if code != 0 {
                    process::exit(code);
                }
            }
            MergeCaptainCommand::Mock(mock) => {
                let code = match mock {
                    MergeCaptainMockCommand::Init(args) => {
                        commands::merge_captain_mock::run_init(&args)
                    }
                    MergeCaptainMockCommand::Step(args) => {
                        commands::merge_captain_mock::run_step(&args)
                    }
                    MergeCaptainMockCommand::Status(args) => {
                        commands::merge_captain_mock::run_status(&args)
                    }
                    MergeCaptainMockCommand::Serve(args) => {
                        commands::merge_captain_mock::run_serve(&args).await
                    }
                    MergeCaptainMockCommand::Cleanup(args) => {
                        commands::merge_captain_mock::run_cleanup(&args)
                    }
                    MergeCaptainMockCommand::Scenarios => {
                        commands::merge_captain_mock::run_scenarios()
                    }
                };
                if code != 0 {
                    process::exit(code);
                }
            }
        },
        Command::Pg(args) => match args.command {
            PgCommand::Codegen(codegen) => {
                let code = commands::pg_codegen::run(&codegen);
                if code != 0 {
                    process::exit(code);
                }
            }
        },
        Command::Persona(args) => {
            if let Err(error) = commands::persona_dispatch::run(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Tool(args) => match args.command {
            ToolCommand::New(new_args) => {
                if let Err(error) = commands::tool::run_new(&new_args).await {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
        },
        // Hidden dev-only generators; see commands::generate.
        Command::DumpHighlightKeywords(_)
        | Command::DumpPromptGrammar(_)
        | Command::DumpTriggerQuickref(_)
        | Command::DumpConnectorMatrix(_)
        | Command::DumpProtocolArtifacts(_)
        | Command::ConnectorSchemaCodegen(_) => commands::generate::dispatch(subcommand),
        Command::Time(args) => match args.command {
            TimeCommand::Run(time_args) => commands::time::run(time_args).await,
        },
    }
}

pub(crate) fn run_profile_options(args: &cli::ProfileArgs) -> commands::run::RunProfileOptions {
    commands::run::RunProfileOptions {
        text: args.text,
        json_path: args.json_path.clone(),
    }
}

pub(crate) fn print_completions(shell: CompletionShell) {
    let mut command = Cli::command();
    let shell = clap_complete::Shell::from(shell);
    clap_complete::generate(shell, &mut command, "harn", &mut std::io::stdout());
}

/// Back-compat shim for the legacy `harn serve [flags] agent.harn` shape,
/// which predates the explicit transport subcommands and defaulted to
/// A2A. When the token after `serve` is not a known transport subcommand
/// (nor a help flag), assume the legacy shape and inject `a2a`.
///
/// The set of transports is read from the clap command tree rather than
/// hard-coded, so a newly added transport (e.g. `site`) is recognized
/// automatically instead of being silently rewritten to `a2a`.
pub(crate) fn normalize_serve_args(mut raw_args: Vec<String>) -> Vec<String> {
    if raw_args.len() > 2 && raw_args.get(1).is_some_and(|arg| arg == "serve") {
        let token = raw_args.get(2).map(String::as_str).unwrap_or_default();
        let is_explicit = token == "-h"
            || token == "--help"
            || serve_subcommand_names().iter().any(|name| name == token);
        if !is_explicit {
            raw_args.insert(2, "a2a".to_string());
        }
    }
    raw_args
}

/// Names of the transport subcommands clap knows under `harn serve`.
pub(crate) fn serve_subcommand_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .find_subcommand("serve")
        .map(|serve| {
            serve
                .get_subcommands()
                .map(|sub| sub.get_name().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Schema version for `harn version --json`. Bump when the data shape
/// changes; new optional fields can be added freely.
pub(crate) const VERSION_SCHEMA_VERSION: u32 = 1;

/// Launch the Harn debug adapter (DAP) over stdio for `harn dap`.
///
/// The adapter runs its own blocking stdio read loop and builds a fresh Tokio
/// runtime per VM step, so it must not execute inside this CLI's Tokio runtime
/// — nesting `Runtime::new()` inside an entered runtime panics. We hand it a
/// dedicated OS thread with no entered runtime context (the same clean footing
/// the `harn-dap` multi-call alias gets from `main`) and block until the
/// client disconnects. Reuses `harn_dap::run` verbatim; no duplicated server.
pub(crate) fn run_dap_adapter() {
    thread::Builder::new()
        .name("harn-dap".to_string())
        .spawn(harn_dap::run)
        .expect("spawn harn-dap adapter thread")
        .join()
        .expect("harn-dap adapter thread panicked");
}

pub(crate) async fn run_version(args: cli::VersionArgs) -> i32 {
    let _name = env_guard::ScopedEnvVar::set("HARN_BUILD_NAME", env!("CARGO_PKG_NAME"));
    let _version = env_guard::ScopedEnvVar::set("HARN_BUILD_VERSION", env!("CARGO_PKG_VERSION"));
    let _description =
        env_guard::ScopedEnvVar::set("HARN_BUILD_DESCRIPTION", env!("CARGO_PKG_DESCRIPTION"));
    let _revision =
        env_guard::ScopedEnvVar::set("HARN_BUILD_REVISION", env!("HARN_BUILD_REVISION"));
    let argv = if args.json {
        vec!["--json".to_string()]
    } else {
        Vec::new()
    };
    dispatch::dispatch_to_embedded_script("version", argv, args.json).await
}

/// Run a `.harn` `@job` once against a JSON request and print the report JSON.
///
/// Lowering through the trigger dispatcher keeps retry, DLQ, budget, and
/// cancellation behavior aligned with long-running worker execution.
pub(crate) async fn run_as_job(args: &cli::RunArgs) {
    let Some(file) = args.file.as_deref() else {
        command_error("`--as-job` requires a `.harn` file path");
    };
    let Some(job) = args.job.as_deref() else {
        command_error("`--as-job` requires `--job <name>`");
    };
    let Some(request) = args.request.as_deref() else {
        command_error("`--as-job` requires `--request <path>`");
    };

    let script_path = PathBuf::from(file);
    let job = job.to_string();
    let request = request.to_path_buf();
    let result_out = args.result_out.clone();

    // Pin the whole job run to the current thread: the trigger registry
    // and dispatcher state are thread-local, so the register → resolve →
    // dispatch sequence must not migrate across the multi-thread runtime's
    // workers between awaits. `LocalSet::run_until` keeps it on one thread
    // and also backs any `spawn_local` the VM performs.
    let local = tokio::task::LocalSet::new();
    let outcome = local
        .run_until(async move {
            harn_serve::run_job_from_files(
                &script_path,
                &job,
                &request,
                result_out.as_deref(),
                false,
            )
            .await
        })
        .await;

    match outcome {
        Ok((outcome, rendered)) => {
            println!("{rendered}");
            if !outcome.succeeded() {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("[harn] job failed: {}", error.message());
            process::exit(1);
        }
    }
}

pub(crate) fn verify_provenance_receipt(path: &str, json: bool) -> Result<(), String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("failed to read {path}: {error}"))?;
    let receipt: harn_vm::ProvenanceReceipt = serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse provenance receipt {path}: {error}"))?;
    let report = harn_vm::verify_receipt(&receipt);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else if report.verified {
        println!(
            "verified receipt={} events={} receipt_hash={} event_root_hash={}",
            report.receipt_id.unwrap_or_else(|| "-".to_string()),
            report.event_count,
            report.receipt_hash.unwrap_or_else(|| "-".to_string()),
            report.event_root_hash.unwrap_or_else(|| "-".to_string())
        );
    } else {
        println!(
            "failed receipt={} events={}",
            report.receipt_id.unwrap_or_else(|| "-".to_string()),
            report.event_count
        );
        for error in &report.errors {
            println!("  {error}");
        }
        return Err("provenance receipt verification failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_serve_args, serve_subcommand_names};

    #[test]
    fn normalize_serve_args_inserts_a2a_for_legacy_shape() {
        let args = normalize_serve_args(vec![
            "harn".to_string(),
            "serve".to_string(),
            "--port".to_string(),
            "3000".to_string(),
            "agent.harn".to_string(),
        ]);
        assert_eq!(
            args,
            vec![
                "harn".to_string(),
                "serve".to_string(),
                "a2a".to_string(),
                "--port".to_string(),
                "3000".to_string(),
                "agent.harn".to_string(),
            ]
        );
    }

    #[test]
    fn normalize_serve_args_preserves_explicit_subcommands() {
        // Every transport clap knows must pass through untouched — a new
        // transport that the shim failed to recognize would be rewritten
        // to `a2a` and mis-parsed (the `site` regression that motivated
        // deriving the list from clap rather than hard-coding it).
        for transport in serve_subcommand_names() {
            let args = normalize_serve_args(vec![
                "harn".to_string(),
                "serve".to_string(),
                transport.clone(),
                "server.harn".to_string(),
            ]);
            assert_eq!(
                args,
                vec![
                    "harn".to_string(),
                    "serve".to_string(),
                    transport.clone(),
                    "server.harn".to_string(),
                ],
                "transport `{transport}` should not be rewritten",
            );
        }
    }

    #[test]
    fn normalize_serve_args_recognizes_site_subcommand() {
        let args = normalize_serve_args(vec![
            "harn".to_string(),
            "serve".to_string(),
            "site".to_string(),
            "server.harn".to_string(),
        ]);
        assert_eq!(args.get(2).map(String::as_str), Some("site"));
    }
}
