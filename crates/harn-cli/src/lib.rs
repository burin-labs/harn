#![recursion_limit = "256"]

pub mod acp;
pub mod cli;
mod cli_bytecode;
pub mod commands;
pub mod config;
#[doc(hidden)]
pub mod dispatch;
pub mod env_guard;
pub mod format;
pub mod json_envelope;
mod net;
pub mod package;
mod provider_bootstrap;
mod runtime;
pub mod skill_loader;
pub mod skill_provenance;
pub mod test_report;
pub mod test_runner;
#[doc(hidden)]
pub mod tests;
mod typecheck_imports;

pub use harn_skills::{get_embedded_skill, list_embedded_skills, EmbeddedSkill, SkillFrontmatter};

use clap::{error::ErrorKind, CommandFactory, Parser as ClapParser};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::{env, fs, panic, process, thread};

use cli::{
    refresh_provider_catalog_if_requested, Cli, Command, CompletionShell, EvalCommand,
    GuardCommand, MergeCaptainCommand, MergeCaptainMockCommand, ModelInfoArgs,
    PackageArtifactsCommand, PackageCacheCommand, PackageCommand, PackageScaffoldCommand,
    PersonaCommand, PersonaSupervisionCommand, PgCommand, ProviderCatalogCommand, ProviderCommand,
    RunsCommand, ServeCommand, SkillCommand, SkillKeyCommand, SkillTrustCommand, TimeCommand,
    ToolCommand,
};
use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};
use runtime::{build_cli_runtime, cli_runtime_mode, CliRuntimeMode};

pub const CLI_RUNTIME_STACK_SIZE: usize = 16 * 1024 * 1024;

static BROKEN_PIPE_PANIC_HOOK: Once = Once::new();

/// Install the macro-emitted builtin signature slice into the
/// `harn_parser` registry the first time any harn-cli entry point parses
/// or typechecks a script.
///
/// Every code path that drives the parser — `run()`, `execute_run()`,
/// `parse_source_file()`, `analyze_file()`, every test harness — funnels
/// through this single helper so the registry is always populated by the
/// time the typechecker reads it. `install_builtin_signatures` is
/// idempotent on identical `&'static` slices, so repeat calls are
/// cheap (a `OnceLock::set` that no-ops after the first success).
///
/// Tests cannot rely on `run()` having executed, so they must reach the
/// parser via one of these entry points (which always do call this).
pub(crate) fn ensure_builtin_signatures_installed() {
    harn_parser::install_builtin_signatures(harn_vm::stdlib::all_builtin_signatures());
}

#[cfg(feature = "hostlib")]
pub(crate) fn install_default_hostlib(vm: &mut harn_vm::Vm) {
    let _ = harn_hostlib::install_default(vm);
    // The `rules` capability lives in its own crate (it depends on
    // `harn-rules`, which depends on `harn-hostlib`, so it can't ship inside
    // `install_default`). Wire it in alongside the defaults.
    harn_rules_hostlib::install(vm);
}

#[cfg(not(feature = "hostlib"))]
pub(crate) fn install_default_hostlib(_vm: &mut harn_vm::Vm) {}

/// Entry point used by `src/main.rs`. Hosts the CLI runtime thread and
/// drives the async dispatcher in `async_main`.
pub fn run() {
    install_broken_pipe_panic_hook();

    // Defeat rlib dead-code stripping of `#[harn_builtin]`-emitted statics
    // (linkme issue #36). Without this touch the linker can drop every
    // builtin's distributed-slice entry, leaving `ALL_BUILTIN_DEFS` empty
    // and surfacing as a swarm of `HARN-NAM-002` errors at first call.
    harn_vm::stdlib::force_link();

    ensure_builtin_signatures_installed();

    let raw_args = normalize_serve_args(env::args().collect());
    let runtime_mode = cli_runtime_mode(&raw_args);

    let handle = thread::Builder::new()
        .name("harn-cli".to_string())
        .stack_size(CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = build_cli_runtime(runtime_mode);
            runtime.block_on(async_main(raw_args, runtime_mode));
            // Drain any queued OTLP exports while the tokio runtime
            // is still alive. The auto-registered `OtelSink` uses a
            // batch processor with `runtime::Tokio`; if we let the
            // runtime drop before this call, in-flight spans never
            // reach the configured collector. No-op when OTel is not
            // configured.
            if let Err(error) = harn_vm::events::shutdown_otel_sink() {
                eprintln!("[harn] OTel exporter shutdown failed: {error}");
            }
        })
        .unwrap_or_else(|error| {
            eprintln!("failed to start CLI runtime thread: {error}");
            process::exit(1);
        });

    if let Err(payload) = handle.join() {
        if runtime::is_broken_pipe_panic_payload(payload.as_ref()) {
            process::exit(0);
        }
        std::panic::resume_unwind(payload);
    }
}

fn install_broken_pipe_panic_hook() {
    BROKEN_PIPE_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if runtime::is_broken_pipe_panic_payload(info.payload()) {
                return;
            }
            previous(info);
        }));
    });
}

#[allow(clippy::large_stack_frames)] // dispatch entrypoint owns full Args + per-feature locals.
async fn async_main(raw_args: Vec<String>, runtime_mode: CliRuntimeMode) {
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
            let sandbox_options = if args.no_sandbox {
                commands::run::RunSandboxOptions::disabled()
            } else {
                commands::run::RunSandboxOptions::default()
                    .with_read_only_roots(args.read_only_root.iter().cloned())
            };
            let json_options = args
                .json
                .then_some(commands::run::RunJsonOptions { quiet: args.quiet });
            let aux_options = commands::run::run_aux_options_from_args(&args);
            let harnpack_options = commands::run::harnpack::HarnpackRunOptions {
                allow_unsigned: args.allow_unsigned,
                dry_run_verify: args.dry_run_verify,
            };

            if let Some(resume_target) = args.resume.as_deref() {
                commands::run::run_resume_with_skill_dirs(
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
                )
                .await;
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
                    fs::write(&tmp_path, &wrapped).unwrap_or_else(|e| {
                        command_error(&format!("failed to write temp file for -e: {e}"))
                    });
                    let tmp_str = tmp_path.to_string_lossy().into_owned();
                    if args.explain_cost {
                        commands::run::run_explain_cost_file_with_skill_dirs(&tmp_str);
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
                            harnpack_options.clone(),
                        )
                        .await;
                    }
                    drop(tmp);
                }
                (None, Some(file)) => {
                    if args.explain_cost {
                        commands::run::run_explain_cost_file_with_skill_dirs(file);
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
                            harnpack_options,
                        )
                        .await;
                    }
                }
                (Some(_), Some(_)) => command_error(
                    "`harn run` accepts either `-e <code>` or `<file.harn>`, not both",
                ),
                (None, None) => command_error(
                    "`harn run` requires `--resume <snapshot>`, `-e <code>`, or `<file.harn>`",
                ),
            }
        }
        Command::Check(args) => {
            let json_format_alias =
                !args.json && matches!(args.format, cli::CheckOutputFormat::Json);
            let matrix_format = if args.json {
                if !matches!(args.format, cli::CheckOutputFormat::Text) {
                    command_error("`harn check` accepts either `--json` or `--format`, not both");
                }
                cli::CheckOutputFormat::Json
            } else {
                args.format
            };
            if args.provider_matrix {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let extensions = package::load_runtime_extensions(&cwd);
                package::install_runtime_extensions(&extensions);
                commands::check::provider_matrix::run(
                    matrix_format,
                    args.filter.as_deref(),
                    json_format_alias,
                );
                return;
            }
            if args.connector_matrix {
                commands::check::connector_matrix::run(
                    matrix_format,
                    args.filter.as_deref(),
                    &args.targets,
                    json_format_alias,
                );
                return;
            }
            let mut target_strings: Vec<String> = args.targets.clone();
            if args.workspace {
                let anchor = target_strings.first().map(Path::new);
                match package::load_workspace_config(anchor) {
                    Some((workspace, manifest_dir)) if !workspace.pipelines.is_empty() => {
                        for pipeline in &workspace.pipelines {
                            let candidate = Path::new(pipeline);
                            let resolved = if candidate.is_absolute() {
                                candidate.to_path_buf()
                            } else {
                                manifest_dir.join(candidate)
                            };
                            target_strings.push(resolved.to_string_lossy().into_owned());
                        }
                    }
                    Some(_) => command_error(
                        "--workspace requires `[workspace].pipelines` in the nearest harn.toml",
                    ),
                    None => command_error(
                        "--workspace could not find a harn.toml walking up from the target(s)",
                    ),
                }
            }
            if target_strings.is_empty() {
                if args.json {
                    print_check_error(
                        "missing_targets",
                        "`harn check` requires at least one target path, or `--workspace` with `[workspace].pipelines`",
                    );
                }
                command_error(
                    "`harn check` requires at least one target path, or `--workspace` with `[workspace].pipelines`",
                );
            }
            for target in &target_strings {
                if let Err(error) = package::validate_runtime_manifest_extensions(Path::new(target))
                {
                    if args.json {
                        print_check_error(
                            "manifest_extension_error",
                            &format!("manifest extension validation failed: {error}"),
                        );
                    }
                    command_error(&format!("manifest extension validation failed: {error}"));
                }
            }
            let targets: Vec<&str> = target_strings.iter().map(String::as_str).collect();
            let files = commands::check::collect_harn_targets(&targets);
            if files.is_empty() {
                if args.json {
                    print_check_error(
                        "no_harn_files",
                        "no .harn or .harn.txt files found under the given target(s)",
                    );
                }
                command_error("no .harn or .harn.txt files found under the given target(s)");
            }
            let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
            let module_graph =
                commands::check::build_module_graph_and_seed_analysis(&files, &mut analysis);
            let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
            let mut should_fail = false;
            let mut json_files = Vec::new();
            for file in &files {
                let mut config = package::load_check_config(Some(file));
                if let Some(path) = args.host_capabilities.as_ref() {
                    config.host_capabilities_path = Some(path.clone());
                }
                if let Some(path) = args.bundle_root.as_ref() {
                    config.bundle_root = Some(path.clone());
                }
                if args.strict_types {
                    config.strict_types = true;
                }
                if let Some(sev) = args.preflight.as_deref() {
                    config.preflight_severity = Some(sev.to_string());
                }
                if args.json {
                    let report = commands::check::check_file_report(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        args.invariants,
                    );
                    should_fail |= report.outcome().should_fail(config.strict);
                    json_files.push(report);
                } else {
                    let outcome = commands::check::check_file_inner(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        args.invariants,
                    );
                    should_fail |= outcome.should_fail(config.strict);
                }
            }
            if args.json {
                let report = commands::check::CheckReport::from_files(json_files);
                let envelope = if should_fail {
                    json_envelope::JsonEnvelope {
                        schema_version: commands::check::CHECK_SCHEMA_VERSION,
                        ok: false,
                        data: Some(report),
                        error: Some(json_envelope::JsonError {
                            code: "check_failed".to_string(),
                            message: "one or more files failed `harn check`".to_string(),
                            details: serde_json::Value::Null,
                        }),
                        warnings: Vec::new(),
                    }
                } else {
                    json_envelope::JsonEnvelope::ok(commands::check::CHECK_SCHEMA_VERSION, report)
                };
                println!("{}", json_envelope::to_string_pretty(&envelope));
                if should_fail {
                    process::exit(1);
                }
                return;
            }
            if should_fail {
                process::exit(1);
            }
        }
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
            let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
            let module_graph =
                commands::check::build_module_graph_and_seed_analysis(&files, &mut analysis);
            let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
            // `.harn`-authored custom lint rules (#2850) run in a sandboxed VM,
            // so they're computed once here in the async handler and merged into
            // each file's diagnostics below. Empty (near-zero cost) when the
            // project declares no `*.lint.harn` rules.
            let script_rule_diags = commands::check::run_project_script_rules(&files).await;
            let script_diags_for = |file: &std::path::Path| -> &[harn_lint::LintDiagnostic] {
                script_rule_diags
                    .get(file)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            };
            if args.json {
                // `--json` always reports without modifying source — `--fix`
                // is intentionally orthogonal to structured output so agents
                // can plan repairs from the report and apply them in a
                // follow-up `harn lint --fix` (or `harn fix apply`).
                let mut should_fail = false;
                let mut json_files: Vec<commands::check::LintFileReport> = Vec::new();
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    let lint_config = commands::check::load_harn_lint_config(file);
                    commands::check::apply_loaded_harn_lint_config(&lint_config, &mut config);
                    let require_header =
                        args.require_file_header || lint_config.require_file_header;
                    let complexity_threshold = lint_config.complexity_threshold;
                    let report = commands::check::lint_file_report(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        require_header,
                        complexity_threshold,
                        &lint_config.persona_step_allowlist,
                        script_diags_for(file.as_path()),
                    );
                    should_fail |= report.outcome().should_fail(config.strict || args.strict);
                    json_files.push(report);
                }
                let report = commands::check::LintReport::from_files(json_files);
                let envelope = if should_fail {
                    json_envelope::JsonEnvelope {
                        schema_version: commands::check::LINT_SCHEMA_VERSION,
                        ok: false,
                        data: Some(report),
                        error: Some(json_envelope::JsonError {
                            code: "lint_failed".to_string(),
                            message: "one or more files failed `harn lint`".to_string(),
                            details: serde_json::Value::Null,
                        }),
                        warnings: Vec::new(),
                    }
                } else {
                    json_envelope::JsonEnvelope::ok(commands::check::LINT_SCHEMA_VERSION, report)
                };
                println!("{}", json_envelope::to_string_pretty(&envelope));
                if should_fail {
                    process::exit(1);
                }
                return;
            }
            if args.fix {
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    let lint_config = commands::check::load_harn_lint_config(file);
                    commands::check::apply_loaded_harn_lint_config(&lint_config, &mut config);
                    let require_header =
                        args.require_file_header || lint_config.require_file_header;
                    let complexity_threshold = lint_config.complexity_threshold;
                    commands::check::lint_fix_file(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        require_header,
                        complexity_threshold,
                        &lint_config.persona_step_allowlist,
                    );
                }
                for file in &prompt_files {
                    let lint_config = commands::check::load_harn_lint_config(file);
                    // The template lint rules don't carry autofix
                    // edits yet (intentionally — see
                    // `template_provider_identity::make_diagnostic`),
                    // so `--fix` is equivalent to a regular run.
                    commands::check::lint_prompt_file_inner(
                        file,
                        lint_config.template_variant_branch_threshold,
                        &lint_config.disabled,
                    );
                }
            } else {
                let mut should_fail = false;
                let mut total_findings = 0usize;
                let mut total_fixable = 0usize;
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    let lint_config = commands::check::load_harn_lint_config(file);
                    commands::check::apply_loaded_harn_lint_config(&lint_config, &mut config);
                    let require_header =
                        args.require_file_header || lint_config.require_file_header;
                    let complexity_threshold = lint_config.complexity_threshold;
                    let outcome = commands::check::lint_file_inner(
                        &mut analysis,
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        require_header,
                        complexity_threshold,
                        &lint_config.persona_step_allowlist,
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
            let loaded = match config::load_for_path(anchor) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: {e}");
                    config::HarnConfig::default()
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
        Command::Models(args) => commands::models::run(args).await,
        Command::Local(args) => commands::local::run(args).await,
        Command::Provider(args) => match args.command {
            ProviderCommand::Capabilities(capabilities) => {
                commands::provider_capabilities::run_or_exit(capabilities);
            }
            ProviderCommand::Catalog(catalog) => match catalog.command {
                ProviderCatalogCommand::Refresh(refresh) => {
                    if let Err(error) = commands::providers::run_refresh(&refresh).await {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Validate(validate) => {
                    if let Err(error) = commands::providers::run_validate(&validate) {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::BuildConfig(build_config) => {
                    if let Err(error) = commands::providers::run_build_config(&build_config) {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::BuildCapabilities(build_capabilities) => {
                    if let Err(error) =
                        commands::providers::run_build_capabilities(&build_capabilities)
                    {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Export(export) => {
                    if let Err(error) = commands::providers::run_export(&export) {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Matrix(matrix) => {
                    if let Err(error) = commands::providers::run_matrix(&matrix) {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Support(support) => {
                    if let Err(error) = commands::provider_support::run(&support) {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Recommend(recommend) => {
                    if let Err(error) = commands::providers::run_recommend(&recommend).await {
                        command_error(&error);
                    }
                }
                ProviderCatalogCommand::Show(show) => {
                    refresh_provider_catalog_if_requested(&show).await;
                    let exit_code = dispatch_provider_catalog(show.available_only).await;
                    if exit_code != 0 {
                        process::exit(exit_code);
                    }
                }
            },
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
            ProviderCommand::ToolProbe(tool_probe) => {
                commands::provider::run_provider_tool_probe(tool_probe).await;
            }
            ProviderCommand::CacheProbe(cache_probe) => {
                commands::provider::run_provider_cache_probe(cache_probe).await;
            }
            ProviderCommand::DispatchExplain(explain) => {
                commands::dispatch_explain::run(&explain);
            }
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
        Command::Serve(args) => match args.command {
            ServeCommand::Acp(args) => {
                if let Err(error) = commands::serve::run_acp_server(&args).await {
                    command_error(&error);
                }
            }
            ServeCommand::A2a(args) => {
                if let Err(error) = commands::serve::run_a2a_server(&args).await {
                    command_error(&error);
                }
            }
            ServeCommand::Api(args) => {
                if let Err(error) = commands::serve::run_api_server(&args).await {
                    command_error(&error);
                }
            }
            ServeCommand::Mcp(args) => {
                if let Err(error) = commands::serve::run_mcp_server(&args).await {
                    command_error(&error);
                }
            }
            ServeCommand::Site(args) => {
                if let Err(error) = commands::serve::run_site_server(&args).await {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
            ServeCommand::Worker(args) => {
                if let Err(error) = commands::serve::run_worker_server(&args).await {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
        },
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
        Command::Runs(args) => match args.command {
            RunsCommand::Inspect(inspect) => {
                inspect_run_record(&inspect.path, inspect.compare.as_deref());
            }
            RunsCommand::View(view) => {
                cli::print_runs_view(&view.path, view.session, view.json);
            }
        },
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
        Command::Persona(args) => match args.command {
            PersonaCommand::New(new) => {
                if let Err(error) = commands::persona_scaffold::run_new(&new) {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Doctor(doctor) => {
                if let Err(error) =
                    commands::persona_doctor::run_doctor(args.manifest.as_deref(), &doctor).await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Check(check) => {
                commands::persona::run_check(args.manifest.as_deref(), &check);
            }
            PersonaCommand::List(list) => {
                commands::persona::run_list(args.manifest.as_deref(), &list);
            }
            PersonaCommand::Inspect(inspect) => {
                commands::persona::run_inspect(args.manifest.as_deref(), &inspect);
            }
            PersonaCommand::Status(status) => {
                if let Err(error) = commands::persona::run_status(
                    args.manifest.as_deref(),
                    &args.state_dir,
                    &status,
                )
                .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Pause(control) => {
                if let Err(error) = commands::persona::run_pause(
                    args.manifest.as_deref(),
                    &args.state_dir,
                    &control,
                )
                .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Resume(control) => {
                if let Err(error) = commands::persona::run_resume(
                    args.manifest.as_deref(),
                    &args.state_dir,
                    &control,
                )
                .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Disable(control) => {
                if let Err(error) = commands::persona::run_disable(
                    args.manifest.as_deref(),
                    &args.state_dir,
                    &control,
                )
                .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Tick(tick) => {
                if let Err(error) =
                    commands::persona::run_tick(args.manifest.as_deref(), &args.state_dir, &tick)
                        .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Trigger(trigger) => {
                if let Err(error) = commands::persona::run_trigger(
                    args.manifest.as_deref(),
                    &args.state_dir,
                    &trigger,
                )
                .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Spend(spend) => {
                if let Err(error) =
                    commands::persona::run_spend(args.manifest.as_deref(), &args.state_dir, &spend)
                        .await
                {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
            PersonaCommand::Supervision(supervision) => match supervision.command {
                PersonaSupervisionCommand::Tail(tail) => {
                    if let Err(error) = commands::persona_supervision::run_tail(
                        args.manifest.as_deref(),
                        &args.state_dir,
                        &tail,
                    )
                    .await
                    {
                        eprintln!("error: {error}");
                        process::exit(1);
                    }
                }
            },
        },
        Command::Tool(args) => match args.command {
            ToolCommand::New(new_args) => {
                if let Err(error) = commands::tool::run_new(&new_args).await {
                    eprintln!("error: {error}");
                    process::exit(1);
                }
            }
        },
        Command::DumpHighlightKeywords(args) => {
            commands::dump_highlight_keywords::run(&args.output, args.check);
        }
        Command::DumpTriggerQuickref(args) => {
            commands::dump_trigger_quickref::run(&args.output, args.check);
        }
        Command::DumpConnectorMatrix(args) => {
            commands::check::connector_matrix::run_docs(&args.output, &args.sources, args.check);
        }
        Command::DumpProtocolArtifacts(args) => {
            commands::dump_protocol_artifacts::run(&args.output_dir, args.check);
        }
        Command::ConnectorSchemaCodegen(args) => {
            let code = commands::connector_schema_codegen::run(&args);
            if code != 0 {
                process::exit(code);
            }
        }
        Command::Time(args) => match args.command {
            TimeCommand::Run(time_args) => commands::time::run(time_args).await,
        },
    }
}

fn run_profile_options(args: &cli::ProfileArgs) -> commands::run::RunProfileOptions {
    commands::run::RunProfileOptions {
        text: args.text,
        json_path: args.json_path.clone(),
    }
}

fn print_completions(shell: CompletionShell) {
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
fn normalize_serve_args(mut raw_args: Vec<String>) -> Vec<String> {
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
fn serve_subcommand_names() -> Vec<String> {
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

/// Run `harn version`. Build-time constants travel to the embedded
/// `.harn` script via scoped env vars rather than a new builtin.
async fn run_version(args: cli::VersionArgs) -> i32 {
    let _name = env_guard::ScopedEnvVar::set("HARN_BUILD_NAME", env!("CARGO_PKG_NAME"));
    let _version = env_guard::ScopedEnvVar::set("HARN_BUILD_VERSION", env!("CARGO_PKG_VERSION"));
    let _description =
        env_guard::ScopedEnvVar::set("HARN_BUILD_DESCRIPTION", env!("CARGO_PKG_DESCRIPTION"));
    let argv = if args.json {
        vec!["--json".to_string()]
    } else {
        Vec::new()
    };
    dispatch::dispatch_to_embedded_script("version", argv, args.json).await
}

pub(crate) async fn print_model_info(args: &ModelInfoArgs) -> bool {
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    let api_key_result = harn_vm::llm::resolve_api_key(&resolved.provider);
    let api_key_set = api_key_result.is_ok();
    let api_key = api_key_result.unwrap_or_default();
    let context_window =
        harn_vm::llm::fetch_provider_max_context(&resolved.provider, &resolved.id, &api_key).await;
    let readiness = local_provider_readiness(&resolved.provider, &resolved.id, &api_key).await;
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let runtime_context_window = catalog
        .as_ref()
        .and_then(|entry| entry.runtime_context_window);
    let capabilities = harn_vm::llm::capabilities::lookup(&resolved.provider, &resolved.id);
    let mut payload = serde_json::json!({
        "alias": args.model,
        "id": resolved.id,
        "provider": resolved.provider,
        "resolved_alias": resolved.alias,
        "tool_format": resolved.tool_format,
        "tier": resolved.tier,
        "api_key_set": api_key_set,
        "context_window": context_window,
        "runtime_context_window": runtime_context_window,
        "readiness": readiness,
        "catalog": catalog,
        "capabilities": {
            "native_tools": capabilities.native_tools,
            "defer_loading": capabilities.defer_loading,
            "tool_search": capabilities.tool_search,
            "max_tools": capabilities.max_tools,
            "prompt_caching": capabilities.prompt_caching,
            "vision": capabilities.vision,
            "vision_supported": capabilities.vision_supported,
            "audio": capabilities.audio,
            "pdf": capabilities.pdf,
            "files_api_supported": capabilities.files_api_supported,
            "json_schema": capabilities.json_schema,
            "prefers_xml_scaffolding": capabilities.prefers_xml_scaffolding,
            "prefers_markdown_scaffolding": capabilities.prefers_markdown_scaffolding,
            "structured_output_mode": capabilities.structured_output_mode,
            "supports_assistant_prefill": capabilities.supports_assistant_prefill,
            "prefers_role_developer": capabilities.prefers_role_developer,
            "prefers_xml_tools": capabilities.prefers_xml_tools,
            "thinking": !capabilities.thinking_modes.is_empty(),
            "thinking_block_style": capabilities.thinking_block_style,
            "thinking_modes": capabilities.thinking_modes,
            "interleaved_thinking_supported": capabilities.interleaved_thinking_supported,
            "anthropic_beta_features": capabilities.anthropic_beta_features,
            "preserve_thinking": capabilities.preserve_thinking,
            "server_parser": capabilities.server_parser,
            "honors_chat_template_kwargs": capabilities.honors_chat_template_kwargs,
            "recommended_endpoint": capabilities.recommended_endpoint,
            "text_tool_wire_format_supported": capabilities.text_tool_wire_format_supported,
            "preferred_tool_format": capabilities.preferred_tool_format,
            "tool_mode_parity": capabilities.tool_mode_parity,
            "tool_mode_parity_notes": capabilities.tool_mode_parity_notes,
        },
        "qc_default_model": harn_vm::llm_config::qc_default_model(&resolved.provider),
    });

    let should_verify = args.verify || args.warm;
    let mut ok = true;
    if should_verify {
        if resolved.provider == "ollama" {
            let mut readiness = harn_vm::llm::OllamaReadinessOptions::new(resolved.id.clone());
            readiness.warm = args.warm;
            readiness.observe_loaded = true;
            readiness.keep_alive = args
                .keep_alive
                .as_deref()
                .and_then(harn_vm::llm::normalize_ollama_keep_alive);
            let result = harn_vm::llm::ollama_readiness(readiness).await;
            ok = result.valid;
            payload["readiness"] = serde_json::to_value(&result).unwrap_or_else(|error| {
                serde_json::json!({
                    "valid": false,
                    "status": "serialization_error",
                    "message": format!("failed to serialize readiness result: {error}"),
                })
            });
        } else {
            ok = false;
            payload["readiness"] = serde_json::json!({
                "valid": false,
                "status": "unsupported_provider",
                "message": format!(
                    "models info --verify is only supported for Ollama models; resolved provider is '{}'",
                    resolved.provider
                ),
                "provider": resolved.provider,
            });
        }
    }

    println!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|error| {
            command_error(&format!("failed to serialize model info: {error}"))
        })
    );
    ok
}

async fn local_provider_readiness(
    provider: &str,
    model: &str,
    api_key: &str,
) -> Option<serde_json::Value> {
    let def = harn_vm::llm_config::provider_config(provider)?;
    if def.auth_style != "none" || !harn_vm::llm::supports_model_readiness_probe(&def) {
        return None;
    }
    let readiness = harn_vm::llm::readiness::probe_provider_readiness_with_options(
        provider,
        harn_vm::llm::readiness::ProviderReadinessOptions {
            requested_model: Some(model),
            base_url_override: None,
            api_key_override: Some(api_key),
        },
    )
    .await;
    Some(serde_json::to_value(readiness).unwrap_or_else(|error| {
        serde_json::json!({
            "ok": false,
            "status": "bad_response",
            "message": format!("failed to serialize readiness result: {error}"),
            "provider": provider,
        })
    }))
}

fn build_provider_catalog_payload(available_only: bool) -> serde_json::Value {
    let provider_names = if available_only {
        harn_vm::llm_config::available_provider_names()
    } else {
        harn_vm::llm_config::provider_names()
    };
    let providers: Vec<_> = provider_names
        .into_iter()
        .filter_map(|name| {
            harn_vm::llm_config::provider_config(&name).map(|def| {
                serde_json::json!({
                    "name": name,
                    "display_name": def.display_name,
                    "icon": def.icon,
                    "base_url": harn_vm::llm_config::resolve_base_url(&def),
                    "base_url_env": def.base_url_env,
                    "auth_style": def.auth_style,
                    "auth_envs": harn_vm::llm_config::auth_env_names(&def.auth_env),
                    "auth_available": harn_vm::llm_config::provider_key_available(&name),
                    "features": def.features,
                    "cost_per_1k_in": def.cost_per_1k_in,
                    "cost_per_1k_out": def.cost_per_1k_out,
                    "latency_p50_ms": def.latency_p50_ms,
                    "performance": def.performance,
                })
            })
        })
        .collect();
    let models: Vec<_> = harn_vm::llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| {
            serde_json::json!({
                "id": id,
                "name": model.name,
                "provider": model.provider,
                "context_window": model.context_window,
                "runtime_context_window": model.runtime_context_window,
                "stream_timeout": model.stream_timeout,
                "capabilities": model.capabilities,
                "pricing": model.pricing,
                "performance": model.performance,
            })
        })
        .collect();
    let aliases: Vec<_> = harn_vm::llm_config::alias_entries()
        .into_iter()
        .map(|(name, alias)| {
            serde_json::json!({
                "name": name,
                "id": alias.id,
                "provider": alias.provider,
                "tool_format": alias.tool_format,
                "tool_calling": harn_vm::llm_config::alias_tool_calling_entry(&name),
            })
        })
        .collect();
    serde_json::json!({
        "providers": providers,
        "known_model_names": harn_vm::llm_config::known_model_names(),
        "available_providers": harn_vm::llm_config::available_provider_names(),
        "aliases": aliases,
        "models": models,
        "qc_defaults": harn_vm::llm_config::qc_defaults(),
    })
}

/// Dispatch shim for `harn provider catalog show`. Aggregation stays in
/// Rust (the script can't reach `llm_config` for the catalog walk);
/// the .harn renderer in `stdlib/cli/providers/catalog.harn` only
/// re-emits the JSON envelope.
///
/// Lock keeps concurrent in-process callers from racing on the global
/// env var the dispatch wedge reads — same pattern as the other
/// partial-port commands (see harn#2305 / #2309).
async fn dispatch_provider_catalog(available_only: bool) -> i32 {
    static DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let payload = build_provider_catalog_payload(available_only);
    let payload_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise provider catalog payload: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_LOCK.lock().await;
    let _payload_guard =
        crate::env_guard::ScopedEnvVar::set("HARN_PROVIDER_CATALOG_PAYLOAD_JSON", &payload_json);
    // `--available-only` doesn't enable JSON; the catalog dump is JSON-
    // only on both impls, but pass `true` so the dispatch wedge sets
    // HARN_OUTPUT_JSON for symmetry with peer scripts.
    crate::dispatch::dispatch_to_embedded_script("providers/catalog", Vec::new(), true).await
}

async fn run_provider_ready(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    json: bool,
) {
    let readiness =
        harn_vm::llm::readiness::probe_provider_readiness(provider, model, base_url).await;
    if json {
        match serde_json::to_string_pretty(&readiness) {
            Ok(payload) => println!("{payload}"),
            Err(error) => command_error(&format!("failed to serialize readiness result: {error}")),
        }
    } else if readiness.ok {
        println!("{}", readiness.message);
    } else {
        eprintln!("{}", readiness.message);
    }
    if !readiness.ok {
        process::exit(1);
    }
}

fn command_error(message: &str) -> ! {
    Cli::command()
        .error(ErrorKind::ValueValidation, message)
        .exit()
}

/// Run a `.harn` `@job` once against a JSON request and print the report JSON.
///
/// Lowering through the trigger dispatcher keeps retry, DLQ, budget, and
/// cancellation behavior aligned with long-running worker execution.
async fn run_as_job(args: &cli::RunArgs) {
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

fn print_check_error(code: &str, message: &str) -> ! {
    let envelope: json_envelope::JsonEnvelope<commands::check::CheckReport> =
        json_envelope::JsonEnvelope::err(commands::check::CHECK_SCHEMA_VERSION, code, message);
    println!("{}", json_envelope::to_string_pretty(&envelope));
    process::exit(1);
}

fn print_lint_error(code: &str, message: &str) -> ! {
    let envelope: json_envelope::JsonEnvelope<commands::check::LintReport> =
        json_envelope::JsonEnvelope::err(commands::check::LINT_SCHEMA_VERSION, code, message);
    println!("{}", json_envelope::to_string_pretty(&envelope));
    process::exit(1);
}

fn verify_provenance_receipt(path: &str, json: bool) -> Result<(), String> {
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

pub(crate) fn load_run_record_or_exit(path: &Path) -> harn_vm::orchestration::RunRecord {
    match harn_vm::orchestration::load_run_record(path) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("Failed to load run record: {error}");
            process::exit(1);
        }
    }
}

fn load_eval_suite_manifest_or_exit(path: &Path) -> harn_vm::orchestration::EvalSuiteManifest {
    harn_vm::orchestration::load_eval_suite_manifest(path).unwrap_or_else(|error| {
        eprintln!("Failed to load eval manifest {}: {error}", path.display());
        process::exit(1);
    })
}

fn load_eval_pack_manifest_or_exit(path: &Path) -> harn_vm::orchestration::EvalPackManifest {
    harn_vm::orchestration::load_eval_pack_manifest(path).unwrap_or_else(|error| {
        eprintln!("Failed to load eval pack {}: {error}", path.display());
        process::exit(1);
    })
}

fn load_persona_eval_ladder_manifest_or_exit(
    path: &Path,
) -> harn_vm::orchestration::PersonaEvalLadderManifest {
    harn_vm::orchestration::load_persona_eval_ladder_manifest(path).unwrap_or_else(|error| {
        eprintln!(
            "Failed to load persona eval ladder {}: {error}",
            path.display()
        );
        process::exit(1);
    })
}

fn file_looks_like_eval_manifest(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("harn.eval.toml") {
        return true;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };
        return toml::from_str::<harn_vm::orchestration::EvalPackManifest>(&content)
            .is_ok_and(|manifest| !manifest.cases.is_empty() || !manifest.ladders.is_empty());
    }
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("_type").and_then(|value| value.as_str()) == Some("eval_suite_manifest")
        || json.get("cases").is_some()
}

fn file_looks_like_eval_pack_manifest(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("harn.eval.toml") {
        return true;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        return file_looks_like_eval_manifest(path);
    }
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("version").is_some()
        && (json.get("cases").is_some() || json.get("ladders").is_some())
        && json.get("_type").and_then(|value| value.as_str()) != Some("eval_suite_manifest")
}

fn file_looks_like_persona_eval_ladder_manifest(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            return false;
        };
        return json.get("_type").and_then(|value| value.as_str())
            == Some("persona_eval_ladder_manifest")
            || json.get("timeout_tiers").is_some()
            || json.get("timeout-tiers").is_some();
    }
    toml::from_str::<harn_vm::orchestration::PersonaEvalLadderManifest>(&content).is_ok_and(
        |manifest| {
            manifest
                .type_name
                .eq_ignore_ascii_case("persona_eval_ladder_manifest")
                || (!manifest.timeout_tiers.is_empty() && manifest.backend.path.is_some())
        },
    )
}

pub(crate) fn collect_run_record_paths(path: &str) -> Vec<PathBuf> {
    let path = Path::new(path);
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .unwrap_or_else(|error| {
                eprintln!("Failed to read run directory {}: {error}", path.display());
                process::exit(1);
            })
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect();
        entries.sort();
        return entries;
    }
    eprintln!("Run path does not exist: {}", path.display());
    process::exit(1);
}

fn print_run_diff(diff: &harn_vm::orchestration::RunDiffReport) {
    println!(
        "Diff: {} -> {} [{} -> {}]",
        diff.left_run_id, diff.right_run_id, diff.left_status, diff.right_status
    );
    println!("Identical: {}", diff.identical);
    println!("Stage diffs: {}", diff.stage_diffs.len());
    println!("Tool diffs: {}", diff.tool_diffs.len());
    println!("Observability diffs: {}", diff.observability_diffs.len());
    println!("Transition delta: {}", diff.transition_count_delta);
    println!("Artifact delta: {}", diff.artifact_count_delta);
    println!("Checkpoint delta: {}", diff.checkpoint_count_delta);
    for stage in &diff.stage_diffs {
        println!("- {} [{}]", stage.node_id, stage.change);
        for detail in &stage.details {
            println!("  {detail}");
        }
    }
    for tool in &diff.tool_diffs {
        println!("- tool {} [{}]", tool.tool_name, tool.args_hash);
        println!("  left: {:?}", tool.left_result);
        println!("  right: {:?}", tool.right_result);
    }
    for item in &diff.observability_diffs {
        println!("- {} [{}]", item.label, item.section);
        for detail in &item.details {
            println!("  {detail}");
        }
    }
}

fn inspect_run_record(path: &str, compare: Option<&str>) {
    let run = load_run_record_or_exit(Path::new(path));
    println!("Run: {}", run.id);
    println!(
        "Workflow: {}",
        run.workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone())
    );
    println!("Status: {}", run.status);
    println!("Task: {}", run.task);
    println!("Stages: {}", run.stages.len());
    println!("Artifacts: {}", run.artifacts.len());
    println!("Transitions: {}", run.transitions.len());
    println!("Checkpoints: {}", run.checkpoints.len());
    println!("HITL questions: {}", run.hitl_questions.len());
    if let Some(observability) = &run.observability {
        println!("Planner rounds: {}", observability.planner_rounds.len());
        println!("Research facts: {}", observability.research_fact_count);
        println!("Workers: {}", observability.worker_lineage.len());
        println!(
            "Action graph: {} nodes / {} edges",
            observability.action_graph_nodes.len(),
            observability.action_graph_edges.len()
        );
        println!(
            "Transcript pointers: {}",
            observability.transcript_pointers.len()
        );
        println!("Daemon events: {}", observability.daemon_events.len());
    }
    if let Some(parent_worker_id) = run
        .metadata
        .get("parent_worker_id")
        .and_then(|value| value.as_str())
    {
        println!("Parent worker: {parent_worker_id}");
    }
    if let Some(parent_stage_id) = run
        .metadata
        .get("parent_stage_id")
        .and_then(|value| value.as_str())
    {
        println!("Parent stage: {parent_stage_id}");
    }
    if run
        .metadata
        .get("delegated")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        println!("Delegated: true");
    }
    println!(
        "Pending nodes: {}",
        if run.pending_nodes.is_empty() {
            "-".to_string()
        } else {
            run.pending_nodes.join(", ")
        }
    );
    println!(
        "Replay fixture: {}",
        if run.replay_fixture.is_some() {
            "embedded"
        } else {
            "derived"
        }
    );
    for stage in &run.stages {
        let worker = stage.metadata.get("worker");
        let worker_suffix = worker
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(|name| format!(" worker={name}"))
            .unwrap_or_default();
        println!(
            "- {} [{}] status={} outcome={} branch={}{}",
            stage.node_id,
            stage.kind,
            stage.status,
            stage.outcome,
            stage.branch.clone().unwrap_or_else(|| "-".to_string()),
            worker_suffix,
        );
        if let Some(worker) = worker {
            if let Some(worker_id) = worker.get("id").and_then(|value| value.as_str()) {
                println!("  worker_id: {worker_id}");
            }
            if let Some(child_run_id) = worker.get("child_run_id").and_then(|value| value.as_str())
            {
                println!("  child_run_id: {child_run_id}");
            }
            if let Some(child_run_path) = worker
                .get("child_run_path")
                .and_then(|value| value.as_str())
            {
                println!("  child_run_path: {child_run_path}");
            }
        }
    }
    if let Some(observability) = &run.observability {
        for round in &observability.planner_rounds {
            println!(
                "- planner {} iterations={} llm_calls={} tools={} research_facts={}",
                round.node_id,
                round.iteration_count,
                round.llm_call_count,
                round.tool_execution_count,
                round.research_facts.len()
            );
        }
        for pointer in &observability.transcript_pointers {
            println!(
                "- transcript {} [{}] available={} {}",
                pointer.label,
                pointer.kind,
                pointer.available,
                pointer
                    .path
                    .clone()
                    .unwrap_or_else(|| pointer.location.clone())
            );
        }
        for event in &observability.daemon_events {
            println!(
                "- daemon {} [{:?}] at {}",
                event.name, event.kind, event.timestamp
            );
            println!("  id: {}", event.daemon_id);
            println!("  persist_path: {}", event.persist_path);
            if let Some(summary) = &event.payload_summary {
                println!("  payload: {summary}");
            }
        }
    }
    if let Some(compare_path) = compare {
        let baseline = load_run_record_or_exit(Path::new(compare_path));
        print_run_diff(&harn_vm::orchestration::diff_run_records(&baseline, &run));
    }
}

fn eval_run_record(
    path: &str,
    compare: Option<&str>,
    structural_experiment: Option<&str>,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) {
    if let Some(experiment) = structural_experiment {
        let path_buf = PathBuf::from(path);
        if !path_buf.is_file() || path_buf.extension().and_then(|ext| ext.to_str()) != Some("harn")
        {
            eprintln!(
                "--structural-experiment currently requires a .harn pipeline path, got {path}"
            );
            process::exit(1);
        }
        if compare.is_some() {
            eprintln!("--compare cannot be combined with --structural-experiment");
            process::exit(1);
        }
        if matches!(llm_mock_mode, commands::run::CliLlmMockMode::Record { .. }) {
            eprintln!("--llm-mock-record cannot be combined with --structural-experiment");
            process::exit(1);
        }
        let path_buf = fs::canonicalize(&path_buf).unwrap_or_else(|error| {
            command_error(&format!(
                "failed to canonicalize structural eval pipeline {}: {error}",
                path_buf.display()
            ))
        });
        run_structural_experiment_eval(&path_buf, experiment, argv, llm_mock_mode);
        return;
    }

    let path_buf = PathBuf::from(path);
    if path_buf.is_file() && file_looks_like_persona_eval_ladder_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with persona eval ladder manifests");
            process::exit(1);
        }
        let manifest = load_persona_eval_ladder_manifest_or_exit(&path_buf);
        let report =
            harn_vm::orchestration::run_persona_eval_ladder(&manifest).unwrap_or_else(|error| {
                eprintln!(
                    "Failed to evaluate persona eval ladder {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            });
        print_persona_ladder_report(&report);
        if !report.pass {
            process::exit(1);
        }
        return;
    }

    if path_buf.is_file() && file_looks_like_eval_pack_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with eval pack manifests");
            process::exit(1);
        }
        let manifest = load_eval_pack_manifest_or_exit(&path_buf);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest_resumable(&manifest, None)
            .unwrap_or_else(|error| {
                eprintln!(
                    "Failed to evaluate eval pack {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            });
        print_eval_pack_report(&report);
        if !report.pass {
            process::exit(1);
        }
        return;
    }

    if path_buf.is_file() && file_looks_like_eval_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with eval suite manifests");
            process::exit(1);
        }
        let manifest = load_eval_suite_manifest_or_exit(&path_buf);
        let suite = harn_vm::orchestration::evaluate_run_suite_manifest(&manifest).unwrap_or_else(
            |error| {
                eprintln!(
                    "Failed to evaluate manifest {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            },
        );
        println!(
            "{} {} passed, {} failed, {} total",
            if suite.pass { "PASS" } else { "FAIL" },
            suite.passed,
            suite.failed,
            suite.total
        );
        for case in &suite.cases {
            println!(
                "- {} [{}] {}",
                case.label.clone().unwrap_or_else(|| case.run_id.clone()),
                case.workflow_id,
                if case.pass { "PASS" } else { "FAIL" }
            );
            if let Some(path) = &case.source_path {
                println!("  path: {path}");
            }
            if let Some(comparison) = &case.comparison {
                println!("  baseline identical: {}", comparison.identical);
                if !comparison.identical {
                    println!(
                        "  baseline status: {} -> {}",
                        comparison.left_status, comparison.right_status
                    );
                }
            }
            for failure in &case.failures {
                println!("  {failure}");
            }
        }
        if !suite.pass {
            process::exit(1);
        }
        return;
    }

    let paths = collect_run_record_paths(path);
    if paths.len() > 1 {
        let mut cases = Vec::new();
        for path in &paths {
            let run = load_run_record_or_exit(path);
            let fixture = run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
            cases.push((run, fixture, Some(path.display().to_string())));
        }
        let suite = harn_vm::orchestration::evaluate_run_suite(cases);
        println!(
            "{} {} passed, {} failed, {} total",
            if suite.pass { "PASS" } else { "FAIL" },
            suite.passed,
            suite.failed,
            suite.total
        );
        for case in &suite.cases {
            println!(
                "- {} [{}] {}",
                case.run_id,
                case.workflow_id,
                if case.pass { "PASS" } else { "FAIL" }
            );
            if let Some(path) = &case.source_path {
                println!("  path: {path}");
            }
            if let Some(comparison) = &case.comparison {
                println!("  baseline identical: {}", comparison.identical);
            }
            for failure in &case.failures {
                println!("  {failure}");
            }
        }
        if !suite.pass {
            process::exit(1);
        }
        return;
    }

    let run = load_run_record_or_exit(&paths[0]);
    let fixture = run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
    let report = harn_vm::orchestration::evaluate_run_against_fixture(&run, &fixture);
    println!("{}", if report.pass { "PASS" } else { "FAIL" });
    println!("Stages: {}", report.stage_count);
    if let Some(compare_path) = compare {
        let baseline = load_run_record_or_exit(Path::new(compare_path));
        print_run_diff(&harn_vm::orchestration::diff_run_records(&baseline, &run));
    }
    if !report.failures.is_empty() {
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
    if !report.pass {
        process::exit(1);
    }
}

fn print_eval_pack_report(report: &harn_vm::orchestration::EvalPackReport) {
    println!(
        "{} {} passed, {} blocking failed, {} warning, {} informational, {} total",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed,
        report.blocking_failed,
        report.warning_failed,
        report.informational_failed,
        report.total
    );
    for case in &report.cases {
        println!(
            "- {} [{}] {} ({})",
            case.label,
            case.workflow_id,
            if case.pass { "PASS" } else { "FAIL" },
            case.severity
        );
        if let Some(path) = &case.source_path {
            println!("  path: {path}");
        }
        if let Some(comparison) = &case.comparison {
            println!("  baseline identical: {}", comparison.identical);
            if !comparison.identical {
                println!(
                    "  baseline status: {} -> {}",
                    comparison.left_status, comparison.right_status
                );
            }
        }
        for failure in &case.failures {
            println!("  {failure}");
        }
        for warning in &case.warnings {
            println!("  warning: {warning}");
        }
        for item in &case.informational {
            println!("  info: {item}");
        }
    }
    for ladder in &report.ladders {
        println!(
            "- ladder {} [{}] {} ({}) first_correct={}/{}",
            ladder.id,
            ladder.persona,
            if ladder.pass { "PASS" } else { "FAIL" },
            ladder.severity,
            ladder.first_correct_route.as_deref().unwrap_or("<none>"),
            ladder.first_correct_tier.as_deref().unwrap_or("<none>")
        );
        println!("  artifacts: {}", ladder.artifact_root);
        for tier in &ladder.tiers {
            println!(
                "  - {} [{}] {} tools={} models={} latency={}ms cost=${:.6}",
                tier.timeout_tier,
                tier.route_id,
                tier.outcome,
                tier.tool_calls,
                tier.model_calls,
                tier.latency_ms,
                tier.cost_usd
            );
            for reason in &tier.degradation_reasons {
                println!("    {reason}");
            }
        }
    }
}

fn print_persona_ladder_report(report: &harn_vm::orchestration::PersonaEvalLadderReport) {
    println!(
        "{} ladder {} passed, {} degraded/looped, {} total",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed,
        report.failed,
        report.total
    );
    println!(
        "first_correct: {}/{}",
        report.first_correct_route.as_deref().unwrap_or("<none>"),
        report.first_correct_tier.as_deref().unwrap_or("<none>")
    );
    println!("artifacts: {}", report.artifact_root);
    for tier in &report.tiers {
        println!(
            "- {} [{}] {} tools={} models={} latency={}ms cost=${:.6}",
            tier.timeout_tier,
            tier.route_id,
            tier.outcome,
            tier.tool_calls,
            tier.model_calls,
            tier.latency_ms,
            tier.cost_usd
        );
        for reason in &tier.degradation_reasons {
            println!("  {reason}");
        }
    }
}

pub(crate) fn run_package_evals() {
    let paths = package::load_package_eval_pack_paths(None).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut all_pass = true;
    for path in &paths {
        println!("Eval pack: {}", path.display());
        let manifest = load_eval_pack_manifest_or_exit(path);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest_resumable(&manifest, None)
            .unwrap_or_else(|error| {
                eprintln!("Failed to evaluate eval pack {}: {error}", path.display());
                process::exit(1);
            });
        print_eval_pack_report(&report);
        all_pass &= report.pass;
    }
    if !all_pass {
        process::exit(1);
    }
}

fn run_structural_experiment_eval(
    path: &Path,
    experiment: &str,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) {
    let baseline_dir = tempfile::Builder::new()
        .prefix("harn-eval-baseline-")
        .tempdir()
        .unwrap_or_else(|error| {
            command_error(&format!("failed to create baseline tempdir: {error}"))
        });
    let variant_dir = tempfile::Builder::new()
        .prefix("harn-eval-variant-")
        .tempdir()
        .unwrap_or_else(|error| {
            command_error(&format!("failed to create variant tempdir: {error}"))
        });

    let baseline = spawn_eval_pipeline_run(path, baseline_dir.path(), None, argv, llm_mock_mode);
    if !baseline.status.success() {
        relay_subprocess_failure("baseline", &baseline);
    }

    let variant = spawn_eval_pipeline_run(
        path,
        variant_dir.path(),
        Some(experiment),
        argv,
        llm_mock_mode,
    );
    if !variant.status.success() {
        relay_subprocess_failure("variant", &variant);
    }

    let baseline_runs = collect_structural_eval_runs(baseline_dir.path());
    let variant_runs = collect_structural_eval_runs(variant_dir.path());
    if baseline_runs.is_empty() || variant_runs.is_empty() {
        eprintln!(
            "structural eval expected workflow run records under {} and {}, but one side was empty",
            baseline_dir.path().display(),
            variant_dir.path().display()
        );
        process::exit(1);
    }
    if baseline_runs.len() != variant_runs.len() {
        eprintln!(
            "structural eval produced different run counts: baseline={} variant={}",
            baseline_runs.len(),
            variant_runs.len()
        );
        process::exit(1);
    }

    let mut baseline_ok = 0usize;
    let mut variant_ok = 0usize;
    let mut any_failures = false;

    println!("Structural experiment: {experiment}");
    println!("Cases: {}", baseline_runs.len());
    for (baseline_run, variant_run) in baseline_runs.iter().zip(variant_runs.iter()) {
        let baseline_fixture = baseline_run
            .replay_fixture
            .clone()
            .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(baseline_run));
        let variant_fixture = variant_run
            .replay_fixture
            .clone()
            .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(variant_run));
        let baseline_report =
            harn_vm::orchestration::evaluate_run_against_fixture(baseline_run, &baseline_fixture);
        let variant_report =
            harn_vm::orchestration::evaluate_run_against_fixture(variant_run, &variant_fixture);
        let diff = harn_vm::orchestration::diff_run_records(baseline_run, variant_run);
        if baseline_report.pass {
            baseline_ok += 1;
        }
        if variant_report.pass {
            variant_ok += 1;
        }
        any_failures |= !baseline_report.pass || !variant_report.pass;
        println!(
            "- {} [{}]",
            variant_run
                .workflow_name
                .clone()
                .unwrap_or_else(|| variant_run.workflow_id.clone()),
            variant_run.task
        );
        println!(
            "  baseline: {}",
            if baseline_report.pass { "PASS" } else { "FAIL" }
        );
        for failure in &baseline_report.failures {
            println!("    {failure}");
        }
        println!(
            "  variant: {}",
            if variant_report.pass { "PASS" } else { "FAIL" }
        );
        for failure in &variant_report.failures {
            println!("    {failure}");
        }
        println!("  diff identical: {}", diff.identical);
        println!("  stage diffs: {}", diff.stage_diffs.len());
        println!("  tool diffs: {}", diff.tool_diffs.len());
        println!("  observability diffs: {}", diff.observability_diffs.len());
    }

    println!("Baseline {} / {} passed", baseline_ok, baseline_runs.len());
    println!("Variant {} / {} passed", variant_ok, variant_runs.len());

    if any_failures {
        process::exit(1);
    }
}

fn spawn_eval_pipeline_run(
    path: &Path,
    run_dir: &Path,
    structural_experiment: Option<&str>,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) -> std::process::Output {
    let exe = env::current_exe().unwrap_or_else(|error| {
        command_error(&format!("failed to resolve current executable: {error}"))
    });
    let mut command = std::process::Command::new(exe);
    command.current_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    command.arg("run");
    match llm_mock_mode {
        commands::run::CliLlmMockMode::Off => {}
        commands::run::CliLlmMockMode::Replay { fixture_path } => {
            command
                .arg("--llm-mock")
                .arg(absolute_cli_path(fixture_path));
        }
        commands::run::CliLlmMockMode::Record { fixture_path } => {
            command
                .arg("--llm-mock-record")
                .arg(absolute_cli_path(fixture_path));
        }
    }
    command.arg(path);
    if !argv.is_empty() {
        command.arg("--");
        command.args(argv);
    }
    command.env(harn_vm::runtime_paths::HARN_RUN_DIR_ENV, run_dir);
    if let Some(experiment) = structural_experiment {
        command.env("HARN_STRUCTURAL_EXPERIMENT", experiment);
    }
    command.output().unwrap_or_else(|error| {
        command_error(&format!(
            "failed to spawn `harn run {}` for structural eval: {error}",
            path.display()
        ))
    })
}

fn absolute_cli_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

fn relay_subprocess_failure(label: &str, output: &std::process::Output) -> ! {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        eprintln!("[{label}] stdout:\n{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprintln!("[{label}] stderr:\n{stderr}");
    }
    process::exit(output.status.code().unwrap_or(1));
}

fn collect_structural_eval_runs(dir: &Path) -> Vec<harn_vm::orchestration::RunRecord> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| {
            command_error(&format!(
                "failed to read structural eval run dir {}: {error}",
                dir.display()
            ))
        })
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut runs: Vec<_> = paths
        .iter()
        .map(|path| load_run_record_or_exit(path))
        .collect();
    runs.sort_by(|left, right| {
        (
            left.started_at.as_str(),
            left.workflow_id.as_str(),
            left.task.as_str(),
        )
            .cmp(&(
                right.started_at.as_str(),
                right.workflow_id.as_str(),
                right.task.as_str(),
            ))
    });
    runs
}

/// Exits on error.
pub(crate) fn parse_source_file(path: &str) -> (String, Vec<harn_parser::SNode>) {
    ensure_builtin_signatures_installed();

    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {path}: {e}");
            process::exit(1);
        }
    };

    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                &source,
                path,
                &error_span_from_lex(&e),
                "error",
                harn_parser::diagnostic::lexer_error_code(&e),
                &e.to_string(),
                Some("here"),
                None,
            );
            eprint!("{diagnostic}");
            process::exit(1);
        }
    };

    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(err) => {
            if parser.all_errors().is_empty() {
                let span = error_span_from_parse(&err);
                let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                    &source,
                    path,
                    &span,
                    "error",
                    harn_parser::diagnostic::parser_error_code(&err),
                    &harn_parser::diagnostic::parser_error_message(&err),
                    Some(harn_parser::diagnostic::parser_error_label(&err)),
                    harn_parser::diagnostic::parser_error_help(&err),
                );
                eprint!("{diagnostic}");
            } else {
                for e in parser.all_errors() {
                    let span = error_span_from_parse(e);
                    let diagnostic = harn_parser::diagnostic::render_diagnostic_with_code(
                        &source,
                        path,
                        &span,
                        "error",
                        harn_parser::diagnostic::parser_error_code(e),
                        &harn_parser::diagnostic::parser_error_message(e),
                        Some(harn_parser::diagnostic::parser_error_label(e)),
                        harn_parser::diagnostic::parser_error_help(e),
                    );
                    eprint!("{diagnostic}");
                }
            }
            process::exit(1);
        }
    };

    (source, program)
}

fn error_span_from_lex(e: &harn_lexer::LexerError) -> harn_lexer::Span {
    match e {
        harn_lexer::LexerError::UnexpectedCharacter(_, span)
        | harn_lexer::LexerError::UnterminatedString(span)
        | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

fn error_span_from_parse(e: &harn_parser::ParserError) -> harn_lexer::Span {
    match e {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
}

/// The pipeline stage at which an `execute_*` call failed. Callers (the
/// conformance harness, the REPL) use this to label a failure accurately
/// instead of calling every failure a "runtime error" — a parse, typecheck,
/// or compile failure never reaches the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecStage {
    Parse,
    Typecheck,
    Compile,
    Runtime,
}

impl ExecStage {
    /// Human-facing label for this stage, e.g. `"type error"`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExecStage::Parse => "parse error",
            ExecStage::Typecheck => "type error",
            ExecStage::Compile => "compile error",
            ExecStage::Runtime => "runtime error",
        }
    }
}

/// An `execute_*` failure tagged with the stage it came from. `Display`
/// renders only the bare message (matching the historical `String` error),
/// so callers that just print `{e}` are unaffected; the `stage` is available
/// for callers that want to label the failure.
#[derive(Debug, Clone)]
pub(crate) struct ExecError {
    pub(crate) stage: ExecStage,
    pub(crate) message: String,
}

impl ExecError {
    fn new(stage: ExecStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Used by REPL and conformance tests.
pub(crate) async fn execute(source: &str, source_path: Option<&Path>) -> Result<String, ExecError> {
    execute_with_skill_dirs(source, source_path, &[]).await
}

pub(crate) async fn execute_with_skill_dirs(
    source: &str,
    source_path: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
) -> Result<String, ExecError> {
    execute_with_skill_dirs_and_optional_harness(source, source_path, cli_skill_dirs, None).await
}

pub(crate) async fn execute_with_skill_dirs_and_harness(
    source: &str,
    source_path: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    harness: harn_vm::Harness,
) -> Result<String, ExecError> {
    execute_with_skill_dirs_and_optional_harness(source, source_path, cli_skill_dirs, Some(harness))
        .await
}

async fn execute_with_skill_dirs_and_optional_harness(
    source: &str,
    source_path: Option<&Path>,
    cli_skill_dirs: &[PathBuf],
    harness: Option<harn_vm::Harness>,
) -> Result<String, ExecError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|e| ExecError::new(ExecStage::Parse, e.to_string()))?;
    let mut parser = Parser::new(tokens);
    let program = parser
        .parse()
        .map_err(|e| ExecError::new(ExecStage::Parse, e.to_string()))?;

    // Static cross-module resolution: when executed from a file, derive the
    // import graph so `execute` catches undefined calls at typecheck time.
    // The REPL / `-e` path invokes this without `source_path`, where there
    // is no importing file context; we fall back to no-imports checking.
    let mut checker = TypeChecker::new();
    if let Some(path) = source_path {
        checker = crate::typecheck_imports::checker_with_resolved_imports(checker, path);
    }
    let type_diagnostics = checker.check(&program);
    let mut warning_lines = Vec::new();
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                return Err(ExecError::new(ExecStage::Typecheck, diag.message.clone()))
            }
            DiagnosticSeverity::Warning => {
                warning_lines.push(format!("warning: {}", diag.message));
            }
        }
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| ExecError::new(ExecStage::Compile, e.to_string()))?;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            install_default_hostlib(&mut vm);
            let source_parent = source_path
                .and_then(|p| p.parent())
                .unwrap_or(std::path::Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            let execution_cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .into_owned();
            let source_dir = source_parent.to_string_lossy().into_owned();
            if source_path.is_some_and(is_conformance_path) {
                harn_vm::event_log::install_memory_for_current_thread(64);
            }
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            let pipeline_name = source_path
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("default");
            harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(execution_cwd),
                    source_dir: Some(source_dir),
                    env: std::collections::BTreeMap::new(),
                    adapter: None,
                    repo_path: None,
                    worktree_path: None,
                    branch: None,
                    base_ref: None,
                    cleanup: None,
                },
            ));
            if let Some(ref root) = project_root {
                vm.set_project_root(root);
            }
            if let Some(path) = source_path {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        vm.set_source_dir(parent);
                    }
                }
            }
            // Conformance tests land here via `run_conformance_tests`; for
            // `skill_fs_*` fixtures to see the bundled `skills/` folder
            // we run the same layered discovery as `harn run`.
            let loaded = skill_loader::load_skills(&skill_loader::SkillLoaderInputs {
                cli_dirs: cli_skill_dirs.to_vec(),
                source_path: source_path.map(Path::to_path_buf),
            });
            skill_loader::emit_loader_warnings(&loaded.loader_warnings);
            skill_loader::install_skills_global(&mut vm, &loaded);
            vm.set_harness(harness.unwrap_or_else(harn_vm::Harness::real));
            if let Some(path) = source_path {
                let extensions = package::load_runtime_extensions(path);
                package::install_runtime_extensions(&extensions);
                package::install_manifest_triggers(&mut vm, &extensions)
                    .await
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to install manifest triggers: {error}"),
                        )
                    })?;
                package::install_manifest_hooks(&mut vm, &extensions)
                    .await
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to install manifest hooks: {error}"),
                        )
                    })?;
            }
            let _event_log = harn_vm::event_log::active_event_log()
                .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
            let connector_clients_installed =
                should_install_default_connector_clients(source, source_path);
            if connector_clients_installed {
                install_default_connector_clients(store_base)
                    .await
                    .map_err(|error| {
                        ExecError::new(
                            ExecStage::Runtime,
                            format!("failed to initialize connector clients: {error}"),
                        )
                    })?;
            }
            let execution_result = vm
                .execute(&chunk)
                .await
                .map_err(|e| ExecError::new(ExecStage::Runtime, e.to_string()));
            harn_vm::egress::reset_egress_policy_for_host();
            if connector_clients_installed {
                harn_vm::clear_active_connector_clients();
            }
            harn_vm::stdlib::process::set_thread_execution_context(None);
            execution_result?;
            let mut output = String::new();
            for wl in &warning_lines {
                output.push_str(wl);
                output.push('\n');
            }
            output.push_str(vm.output());
            Ok(output)
        })
        .await
}

fn should_install_default_connector_clients(source: &str, source_path: Option<&Path>) -> bool {
    if !source_path.is_some_and(is_conformance_path) {
        return true;
    }
    source.contains("connector_call")
        || source.contains("std/connectors")
        || source.contains("connectors/")
}

fn is_conformance_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "conformance")
}

async fn install_default_connector_clients(base_dir: &Path) -> Result<(), String> {
    let event_log = harn_vm::event_log::active_event_log()
        .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
    let secret_namespace = connector_secret_namespace(base_dir);
    let secrets: Arc<dyn harn_vm::secrets::SecretProvider> = Arc::new(
        harn_vm::secrets::configured_default_chain(secret_namespace)
            .map_err(|error| format!("failed to configure secret providers: {error}"))?,
    );

    let registry = harn_vm::ConnectorRegistry::default();
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox = Arc::new(
        harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
            .await
            .map_err(|error| error.to_string())?,
    );
    registry
        .init_all(harn_vm::ConnectorCtx {
            event_log,
            secrets,
            inbox,
            metrics,
            rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
        })
        .await
        .map_err(|error| error.to_string())?;
    let clients = registry.client_map().await;
    harn_vm::install_active_connector_clients(clients);
    Ok(())
}

fn connector_secret_namespace(base_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = base_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}

#[cfg(test)]
mod main_tests {
    use super::{
        normalize_serve_args, serve_subcommand_names, should_install_default_connector_clients,
    };
    use std::path::Path;

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

    #[test]
    fn conformance_skips_connector_clients_unless_fixture_uses_connectors() {
        let path = Path::new("conformance/tests/language/basic.harn");
        assert!(!should_install_default_connector_clients(
            "__io_println(1)",
            Some(path)
        ));
        assert!(!should_install_default_connector_clients(
            "trust_graph_verify_chain()",
            Some(path)
        ));
        assert!(should_install_default_connector_clients(
            "import { post_message } from \"std/connectors/slack\"",
            Some(path)
        ));
        assert!(should_install_default_connector_clients(
            "__io_println(1)",
            Some(Path::new("examples/demo.harn"))
        ));
    }
}
