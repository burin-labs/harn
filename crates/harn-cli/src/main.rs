mod acp;
mod cli;
mod commands;
mod config;
mod env_guard;
mod format;
mod package;
mod skill_loader;
mod skill_provenance;
mod test_runner;
#[cfg(test)]
mod tests;

use clap::{error::ErrorKind, CommandFactory, Parser as ClapParser};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{env, fs, process, thread};

use cli::{
    Cli, Command, ModelInfoArgs, PackageCacheCommand, PackageCommand, PersonaCommand, RunsCommand,
    ServeCommand, SkillCommand, SkillKeyCommand, SkillTrustCommand, SkillsCommand,
};
use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

const CLI_RUNTIME_STACK_SIZE: usize = 16 * 1024 * 1024;

fn main() {
    let handle = thread::Builder::new()
        .name("harn-cli".to_string())
        .stack_size(CLI_RUNTIME_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|error| {
                    eprintln!("failed to start async runtime: {error}");
                    process::exit(1);
                });
            runtime.block_on(async_main());
        })
        .unwrap_or_else(|error| {
            eprintln!("failed to start CLI runtime thread: {error}");
            process::exit(1);
        });

    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

async fn async_main() {
    let raw_args = normalize_serve_args(env::args().collect());
    if raw_args.len() == 2 && raw_args[1].ends_with(".harn") {
        commands::run::run_file(
            &raw_args[1],
            false,
            std::collections::HashSet::new(),
            Vec::new(),
            commands::run::CliLlmMockMode::Off,
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

    match cli.command.expect("clap requires a command") {
        Command::Version => print_version(),
        Command::Skill(args) => match args.command {
            SkillCommand::Key(key_args) => match key_args.command {
                SkillKeyCommand::Generate(generate) => commands::skill::run_key_generate(&generate),
            },
            SkillCommand::Sign(sign) => commands::skill::run_sign(&sign),
            SkillCommand::Verify(verify) => commands::skill::run_verify(&verify),
            SkillCommand::Trust(trust_args) => match trust_args.command {
                SkillTrustCommand::Add(add) => commands::skill::run_trust_add(&add),
                SkillTrustCommand::List(list) => commands::skill::run_trust_list(&list),
            },
        },
        Command::Run(args) => {
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

            match (args.eval.as_deref(), args.file.as_deref()) {
                (Some(code), None) => {
                    let wrapped = format!("pipeline main(task) {{\n{code}\n}}");
                    // Unique filename avoids collisions between concurrent
                    // `harn run -e` invocations; Drop guards cleanup on
                    // panic. The `.harn` suffix keeps tree-sitter and
                    // pipeline dispatch matching on extension.
                    let tmp = tempfile::Builder::new()
                        .prefix("harn-eval-")
                        .suffix(".harn")
                        .tempfile()
                        .unwrap_or_else(|e| {
                            command_error(&format!("failed to create temp file for -e: {e}"))
                        });
                    let tmp_path: PathBuf = tmp.path().to_path_buf();
                    fs::write(&tmp_path, &wrapped).unwrap_or_else(|e| {
                        command_error(&format!("failed to write temp file for -e: {e}"))
                    });
                    let tmp_str = tmp_path.to_string_lossy().into_owned();
                    commands::run::run_file_with_skill_dirs(
                        &tmp_str,
                        args.trace,
                        denied,
                        args.argv.clone(),
                        args.skill_dir.clone(),
                        llm_mock_mode.clone(),
                    )
                    .await;
                    drop(tmp);
                }
                (None, Some(file)) => {
                    commands::run::run_file_with_skill_dirs(
                        file,
                        args.trace,
                        denied,
                        args.argv.clone(),
                        args.skill_dir.clone(),
                        llm_mock_mode,
                    )
                    .await
                }
                (Some(_), Some(_)) => command_error(
                    "`harn run` accepts either `-e <code>` or `<file.harn>`, not both",
                ),
                (None, None) => {
                    command_error("`harn run` requires either `-e <code>` or `<file.harn>`")
                }
            }
        }
        Command::Check(args) => {
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
                command_error(
                    "`harn check` requires at least one target path, or `--workspace` with `[workspace].pipelines`",
                );
            }
            let targets: Vec<&str> = target_strings.iter().map(String::as_str).collect();
            let files = commands::check::collect_harn_targets(&targets);
            if files.is_empty() {
                command_error("no .harn files found under the given target(s)");
            }
            let module_graph = commands::check::build_module_graph(&files);
            let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
            let mut should_fail = false;
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
                let outcome = commands::check::check_file_inner(
                    file,
                    &config,
                    &cross_file_imports,
                    &module_graph,
                    args.invariants,
                );
                should_fail |= outcome.should_fail(config.strict);
            }
            if should_fail {
                process::exit(1);
            }
        }
        Command::Explain(args) => {
            let code = commands::explain::run_explain(&args);
            if code != 0 {
                process::exit(code);
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
            let files = commands::check::collect_harn_targets(&targets);
            if files.is_empty() {
                command_error("no .harn files found under the given target(s)");
            }
            let module_graph = commands::check::build_module_graph(&files);
            let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
            if args.fix {
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    commands::check::apply_harn_lint_config(file, &mut config);
                    let require_header = args.require_file_header
                        || commands::check::harn_lint_require_file_header(file);
                    let complexity_threshold =
                        commands::check::harn_lint_complexity_threshold(file);
                    commands::check::lint_fix_file(
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        require_header,
                        complexity_threshold,
                    );
                }
            } else {
                let mut should_fail = false;
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    commands::check::apply_harn_lint_config(file, &mut config);
                    let require_header = args.require_file_header
                        || commands::check::harn_lint_require_file_header(file);
                    let complexity_threshold =
                        commands::check::harn_lint_complexity_threshold(file);
                    let outcome = commands::check::lint_file_inner(
                        file,
                        &config,
                        &cross_file_imports,
                        &module_graph,
                        require_header,
                        complexity_threshold,
                    );
                    should_fail |= outcome.should_fail(config.strict);
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
            commands::check::fmt_targets(
                &targets,
                commands::check::FmtMode::from_check_flag(args.check),
                &opts,
            );
        }
        Command::Test(args) => {
            if args.target.as_deref() == Some("agents-conformance") {
                if args.selection.is_some() {
                    command_error(
                        "`harn test agents-conformance` does not accept a second positional target; use --category instead",
                    );
                }
                if args.evals || args.determinism || args.record || args.replay || args.watch {
                    command_error(
                        "`harn test agents-conformance` cannot be combined with --evals, --determinism, --record, --replay, or --watch",
                    );
                }
                let Some(target_url) = args.agents_target.clone() else {
                    command_error("`harn test agents-conformance` requires --target <url>");
                };
                commands::agents_conformance::run_agents_conformance(
                    commands::agents_conformance::AgentsConformanceConfig {
                        target_url,
                        api_key: args.agents_api_key.clone(),
                        categories: args.agents_category.clone(),
                        timeout_ms: args.timeout,
                        verbose: args.verbose,
                        json: args.json,
                        json_out: args.json_out.clone(),
                        workspace_id: args.agents_workspace_id.clone(),
                        session_id: args.agents_session_id.clone(),
                    },
                )
                .await;
                return;
            }
            if args.evals {
                if args.determinism || args.record || args.replay || args.watch {
                    command_error("--evals cannot be combined with --determinism, --record, --replay, or --watch");
                }
                if args.target.as_deref() != Some("package") || args.selection.is_some() {
                    command_error("package evals are run with `harn test package --evals`");
                }
                run_package_evals();
            } else if args.determinism {
                if args.watch {
                    command_error("--determinism cannot be combined with --watch");
                }
                if args.record || args.replay {
                    command_error("--determinism manages its own record/replay cycle");
                }
                if let Some(t) = args.target.as_deref() {
                    if t == "conformance" {
                        commands::test::run_conformance_determinism_tests(
                            t,
                            args.selection.as_deref(),
                            args.filter.as_deref(),
                            args.timeout,
                        )
                        .await;
                    } else if args.selection.is_some() {
                        command_error(
                            "only `harn test conformance` accepts a second positional target",
                        );
                    } else {
                        commands::test::run_determinism_tests(
                            t,
                            args.filter.as_deref(),
                            args.timeout,
                        )
                        .await;
                    }
                } else {
                    let test_dir = if PathBuf::from("tests").is_dir() {
                        "tests".to_string()
                    } else {
                        command_error("no path specified and no tests/ directory found");
                    };
                    if args.selection.is_some() {
                        command_error(
                            "only `harn test conformance` accepts a second positional target",
                        );
                    }
                    commands::test::run_determinism_tests(
                        &test_dir,
                        args.filter.as_deref(),
                        args.timeout,
                    )
                    .await;
                }
            } else {
                if args.record {
                    harn_vm::llm::set_replay_mode(
                        harn_vm::llm::LlmReplayMode::Record,
                        ".harn-fixtures",
                    );
                } else if args.replay {
                    harn_vm::llm::set_replay_mode(
                        harn_vm::llm::LlmReplayMode::Replay,
                        ".harn-fixtures",
                    );
                }

                if let Some(t) = args.target.as_deref() {
                    if t == "conformance" {
                        commands::test::run_conformance_tests(
                            t,
                            args.selection.as_deref(),
                            args.filter.as_deref(),
                            args.junit.as_deref(),
                            args.timeout,
                            args.verbose,
                            args.timing,
                        )
                        .await;
                    } else if args.selection.is_some() {
                        command_error(
                            "only `harn test conformance` accepts a second positional target",
                        );
                    } else if args.watch {
                        commands::test::run_watch_tests(
                            t,
                            args.filter.as_deref(),
                            args.timeout,
                            args.parallel,
                        )
                        .await;
                    } else {
                        commands::test::run_user_tests(
                            t,
                            args.filter.as_deref(),
                            args.timeout,
                            args.parallel,
                        )
                        .await;
                    }
                } else {
                    let test_dir = if PathBuf::from("tests").is_dir() {
                        "tests".to_string()
                    } else {
                        command_error("no path specified and no tests/ directory found");
                    };
                    if args.selection.is_some() {
                        command_error(
                            "only `harn test conformance` accepts a second positional target",
                        );
                    }
                    if args.watch {
                        commands::test::run_watch_tests(
                            &test_dir,
                            args.filter.as_deref(),
                            args.timeout,
                            args.parallel,
                        )
                        .await;
                    } else {
                        commands::test::run_user_tests(
                            &test_dir,
                            args.filter.as_deref(),
                            args.timeout,
                            args.parallel,
                        )
                        .await;
                    }
                }
            }
        }
        Command::Init(args) => commands::init::init_project(args.name.as_deref(), args.template),
        Command::New(args) => match commands::init::resolve_new_args(&args) {
            Ok((name, template)) => commands::init::init_project(name.as_deref(), template),
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        },
        Command::Doctor(args) => commands::doctor::run_doctor(!args.no_network).await,
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
            ServeCommand::Mcp(args) => {
                if let Err(error) = commands::serve::run_mcp_server(&args).await {
                    command_error(&error);
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
        Command::Portal(args) => {
            commands::portal::run_portal(&args.dir, &args.host, args.port, args.open).await
        }
        Command::Trigger(args) => {
            if let Err(error) = commands::trigger::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
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
        Command::Trust(args) | Command::TrustGraph(args) => {
            if let Err(error) = commands::trust::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Orchestrator(args) => {
            if let Err(error) = commands::orchestrator::handle(args).await {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        Command::Playground(args) => {
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
                inspect_run_record(&inspect.path, inspect.compare.as_deref())
            }
        },
        Command::Replay(args) => replay_run_record(&args.path),
        Command::Eval(args) => {
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
                &args.path,
                args.compare.as_deref(),
                args.structural_experiment.as_deref(),
                &args.argv,
                &llm_mock_mode,
            )
        }
        Command::Repl => commands::repl::run_repl().await,
        Command::Bench(args) => commands::bench::run_bench(&args.file, args.iterations).await,
        Command::Viz(args) => commands::viz::run_viz(&args.file, args.output.as_deref()),
        Command::Install(args) => package::install_packages(
            args.frozen || args.locked || args.offline,
            args.refetch.as_deref(),
            args.offline,
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
        Command::Update(args) => package::update_packages(args.alias.as_deref(), args.all),
        Command::Remove(args) => package::remove_package(&args.alias),
        Command::Lock => package::lock_packages(),
        Command::Package(args) => match args.command {
            PackageCommand::Search(search) => package::search_package_registry(
                search.query.as_deref(),
                search.registry.as_deref(),
                search.json,
            ),
            PackageCommand::Info(info) => {
                package::show_package_registry_info(&info.name, info.registry.as_deref(), info.json)
            }
            PackageCommand::Check(check) => {
                package::check_package(check.package.as_deref(), check.json)
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
                    package::verify_package_cache(verify.materialized)
                }
            },
        },
        Command::Publish(args) => package::publish_package(
            args.package.as_deref(),
            args.dry_run,
            args.registry.as_deref(),
            args.json,
        ),
        Command::Persona(args) => match args.command {
            PersonaCommand::List(list) => {
                commands::persona::run_list(args.manifest.as_deref(), &list)
            }
            PersonaCommand::Inspect(inspect) => {
                commands::persona::run_inspect(args.manifest.as_deref(), &inspect)
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
        },
        Command::ModelInfo(args) => {
            if !print_model_info(&args).await {
                process::exit(1);
            }
        }
        Command::ProviderCatalog(args) => print_provider_catalog(args.available_only),
        Command::ProviderReady(args) => {
            run_provider_ready(
                &args.provider,
                args.model.as_deref(),
                args.base_url.as_deref(),
                args.json,
            )
            .await
        }
        Command::Skills(args) => match args.command {
            SkillsCommand::List(list) => commands::skills::run_list(&list),
            SkillsCommand::Inspect(inspect) => commands::skills::run_inspect(&inspect),
            SkillsCommand::Match(matcher) => commands::skills::run_match(&matcher),
            SkillsCommand::Install(install) => commands::skills::run_install(&install),
            SkillsCommand::New(new_args) => commands::skills::run_new(&new_args),
        },
        Command::DumpHighlightKeywords(args) => {
            commands::dump_highlight_keywords::run(&args.output, args.check);
        }
        Command::DumpTriggerQuickref(args) => {
            commands::dump_trigger_quickref::run(&args.output, args.check);
        }
    }
}

fn normalize_serve_args(mut raw_args: Vec<String>) -> Vec<String> {
    if raw_args.len() > 2
        && raw_args.get(1).is_some_and(|arg| arg == "serve")
        && !matches!(
            raw_args.get(2).map(String::as_str),
            Some("acp" | "a2a" | "mcp" | "-h" | "--help")
        )
    {
        raw_args.insert(2, "a2a".to_string());
    }
    raw_args
}

fn print_version() {
    println!(
        r#"
 ╱▔▔╲
 ╱    ╲    harn v{}
 │ ◆  │    the agent harness language
 │    │
 ╰──╯╱
   ╱╱
"#,
        env!("CARGO_PKG_VERSION")
    );
}

async fn print_model_info(args: &ModelInfoArgs) -> bool {
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    let api_key_result = harn_vm::llm::resolve_api_key(&resolved.provider);
    let api_key_set = api_key_result.is_ok();
    let api_key = api_key_result.unwrap_or_default();
    let context_window =
        harn_vm::llm::fetch_provider_max_context(&resolved.provider, &resolved.id, &api_key).await;
    let readiness = local_openai_readiness(&resolved.provider, &resolved.id, &api_key).await;
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
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
        "readiness": readiness,
        "catalog": catalog,
        "capabilities": {
            "native_tools": capabilities.native_tools,
            "defer_loading": capabilities.defer_loading,
            "tool_search": capabilities.tool_search,
            "max_tools": capabilities.max_tools,
            "prompt_caching": capabilities.prompt_caching,
            "thinking": capabilities.thinking,
            "preserve_thinking": capabilities.preserve_thinking,
            "server_parser": capabilities.server_parser,
            "honors_chat_template_kwargs": capabilities.honors_chat_template_kwargs,
            "recommended_endpoint": capabilities.recommended_endpoint,
            "text_tool_wire_format_supported": capabilities.text_tool_wire_format_supported,
        },
        "qc_default_model": harn_vm::llm_config::qc_default_model(&resolved.provider),
    });

    let should_verify = args.verify || args.warm;
    let mut ok = true;
    if should_verify {
        if resolved.provider == "ollama" {
            let mut readiness = harn_vm::llm::OllamaReadinessOptions::new(resolved.id.clone());
            readiness.warm = args.warm;
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
                    "model-info --verify is only supported for Ollama models; resolved provider is '{}'",
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

async fn local_openai_readiness(
    provider: &str,
    model: &str,
    api_key: &str,
) -> Option<serde_json::Value> {
    let def = harn_vm::llm_config::provider_config(provider)?;
    if def.auth_style != "none" || !harn_vm::llm::supports_model_readiness_probe(&def) {
        return None;
    }
    let readiness = harn_vm::llm::probe_openai_compatible_model(provider, model, api_key).await;
    Some(serde_json::json!({
        "valid": readiness.valid,
        "category": readiness.category,
        "message": readiness.message,
        "provider": readiness.provider,
        "model": readiness.model,
        "url": readiness.url,
        "status": readiness.status,
        "available_models": readiness.available_models,
    }))
}

fn print_provider_catalog(available_only: bool) {
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
                "stream_timeout": model.stream_timeout,
                "capabilities": model.capabilities,
                "pricing": model.pricing,
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
            })
        })
        .collect();
    let payload = serde_json::json!({
        "providers": providers,
        "known_model_names": harn_vm::llm_config::known_model_names(),
        "available_providers": harn_vm::llm_config::available_provider_names(),
        "aliases": aliases,
        "models": models,
        "qc_defaults": harn_vm::llm_config::qc_defaults(),
    });
    println!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|error| {
            command_error(&format!("failed to serialize provider catalog: {error}"))
        })
    );
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

fn load_run_record_or_exit(path: &Path) -> harn_vm::orchestration::RunRecord {
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

fn file_looks_like_eval_manifest(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("harn.eval.toml") {
        return true;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };
        return toml::from_str::<harn_vm::orchestration::EvalPackManifest>(&content)
            .is_ok_and(|manifest| !manifest.cases.is_empty());
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
        && json.get("cases").is_some()
        && json.get("_type").and_then(|value| value.as_str()) != Some("eval_suite_manifest")
}

fn collect_run_record_paths(path: &str) -> Vec<PathBuf> {
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
            println!("  {}", detail);
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
            println!("  {}", detail);
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
        println!("Parent worker: {}", parent_worker_id);
    }
    if let Some(parent_stage_id) = run
        .metadata
        .get("parent_stage_id")
        .and_then(|value| value.as_str())
    {
        println!("Parent stage: {}", parent_stage_id);
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
                println!("  worker_id: {}", worker_id);
            }
            if let Some(child_run_id) = worker.get("child_run_id").and_then(|value| value.as_str())
            {
                println!("  child_run_id: {}", child_run_id);
            }
            if let Some(child_run_path) = worker
                .get("child_run_path")
                .and_then(|value| value.as_str())
            {
                println!("  child_run_path: {}", child_run_path);
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
                println!("  payload: {}", summary);
            }
        }
    }
    if let Some(compare_path) = compare {
        let baseline = load_run_record_or_exit(Path::new(compare_path));
        print_run_diff(&harn_vm::orchestration::diff_run_records(&baseline, &run));
    }
}

fn replay_run_record(path: &str) {
    let run = load_run_record_or_exit(Path::new(path));
    println!("Replay: {}", run.id);
    for stage in &run.stages {
        println!(
            "[{}] status={} outcome={} branch={}",
            stage.node_id,
            stage.status,
            stage.outcome,
            stage.branch.clone().unwrap_or_else(|| "-".to_string())
        );
        if let Some(text) = &stage.visible_text {
            println!("  visible: {}", text);
        }
        if let Some(verification) = &stage.verification {
            println!("  verification: {}", verification);
        }
    }
    if let Some(transcript) = &run.transcript {
        println!(
            "Transcript events persisted: {}",
            transcript["events"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0)
        );
    }
    let fixture = run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
    let report = harn_vm::orchestration::evaluate_run_against_fixture(&run, &fixture);
    println!(
        "Embedded replay fixture: {}",
        if report.pass { "PASS" } else { "FAIL" }
    );
    for transition in &run.transitions {
        println!(
            "transition {} -> {} ({})",
            transition
                .from_node_id
                .clone()
                .unwrap_or_else(|| "start".to_string()),
            transition.to_node_id,
            transition
                .branch
                .clone()
                .unwrap_or_else(|| "default".to_string())
        );
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
                "--structural-experiment currently requires a .harn pipeline path, got {}",
                path
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
    if path_buf.is_file() && file_looks_like_eval_pack_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with eval pack manifests");
            process::exit(1);
        }
        let manifest = load_eval_pack_manifest_or_exit(&path_buf);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest(&manifest).unwrap_or_else(
            |error| {
                eprintln!(
                    "Failed to evaluate eval pack {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            },
        );
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
                println!("  path: {}", path);
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
                println!("  {}", failure);
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
                println!("  path: {}", path);
            }
            if let Some(comparison) = &case.comparison {
                println!("  baseline identical: {}", comparison.identical);
            }
            for failure in &case.failures {
                println!("  {}", failure);
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
            println!("- {}", failure);
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
            println!("  path: {}", path);
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
            println!("  {}", failure);
        }
        for warning in &case.warnings {
            println!("  warning: {}", warning);
        }
        for item in &case.informational {
            println!("  info: {}", item);
        }
    }
}

fn run_package_evals() {
    let paths = package::load_package_eval_pack_paths(None).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut all_pass = true;
    for path in &paths {
        println!("Eval pack: {}", path.display());
        let manifest = load_eval_pack_manifest_or_exit(path);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest(&manifest).unwrap_or_else(
            |error| {
                eprintln!("Failed to evaluate eval pack {}: {error}", path.display());
                process::exit(1);
            },
        );
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

    println!("Structural experiment: {}", experiment);
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
            println!("    {}", failure);
        }
        println!(
            "  variant: {}",
            if variant_report.pass { "PASS" } else { "FAIL" }
        );
        for failure in &variant_report.failures {
            println!("    {}", failure);
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

/// Parse a .harn file, returning (source, AST). Exits on error.
pub(crate) fn parse_source_file(path: &str) -> (String, Vec<harn_parser::SNode>) {
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
            let diagnostic = harn_parser::diagnostic::render_diagnostic(
                &source,
                path,
                &error_span_from_lex(&e),
                "error",
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
                let diagnostic = harn_parser::diagnostic::render_diagnostic(
                    &source,
                    path,
                    &span,
                    "error",
                    &harn_parser::diagnostic::parser_error_message(&err),
                    Some(harn_parser::diagnostic::parser_error_label(&err)),
                    harn_parser::diagnostic::parser_error_help(&err),
                );
                eprint!("{diagnostic}");
            } else {
                for e in parser.all_errors() {
                    let span = error_span_from_parse(e);
                    let diagnostic = harn_parser::diagnostic::render_diagnostic(
                        &source,
                        path,
                        &span,
                        "error",
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
        | harn_lexer::LexerError::UnterminatedBlockComment(span) => *span,
    }
}

fn error_span_from_parse(e: &harn_parser::ParserError) -> harn_lexer::Span {
    match e {
        harn_parser::ParserError::Unexpected { span, .. } => *span,
        harn_parser::ParserError::UnexpectedEof { span, .. } => *span,
    }
}

/// Execute source code and return the output. Used by REPL and conformance tests.
pub(crate) async fn execute(source: &str, source_path: Option<&Path>) -> Result<String, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    // Static cross-module resolution: when executed from a file, derive the
    // import graph so `execute` catches undefined calls at typecheck time.
    // The REPL / `-e` path invokes this without `source_path`, where there
    // is no importing file context; we fall back to no-imports checking.
    let mut checker = TypeChecker::new();
    if let Some(path) = source_path {
        let graph = harn_modules::build(&[path.to_path_buf()]);
        if let Some(imported) = graph.imported_names_for_file(path) {
            checker = checker.with_imported_names(imported);
        }
        if let Some(imported) = graph.imported_type_declarations_for_file(path) {
            checker = checker.with_imported_type_decls(imported);
        }
    }
    let type_diagnostics = checker.check(&program);
    let mut warning_lines = Vec::new();
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => return Err(diag.message.clone()),
            DiagnosticSeverity::Warning => {
                warning_lines.push(format!("warning: {}", diag.message));
            }
        }
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| e.to_string())?;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
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
                cli_dirs: Vec::new(),
                source_path: source_path.map(Path::to_path_buf),
            });
            skill_loader::emit_loader_warnings(&loaded.loader_warnings);
            skill_loader::install_skills_global(&mut vm, &loaded);
            if let Some(path) = source_path {
                let extensions = package::load_runtime_extensions(path);
                package::install_runtime_extensions(&extensions);
                package::install_manifest_triggers(&mut vm, &extensions)
                    .await
                    .map_err(|error| format!("failed to install manifest triggers: {error}"))?;
                package::install_manifest_hooks(&mut vm, &extensions)
                    .await
                    .map_err(|error| format!("failed to install manifest hooks: {error}"))?;
            }
            let _event_log = harn_vm::event_log::active_event_log()
                .unwrap_or_else(|| harn_vm::event_log::install_memory_for_current_thread(64));
            let connector_clients_installed =
                should_install_default_connector_clients(source, source_path);
            if connector_clients_installed {
                install_default_connector_clients(store_base)
                    .await
                    .map_err(|error| format!("failed to initialize connector clients: {error}"))?;
            }
            let execution_result = vm.execute(&chunk).await.map_err(|e| e.to_string());
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
    use super::{normalize_serve_args, should_install_default_connector_clients};
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
        let args = normalize_serve_args(vec![
            "harn".to_string(),
            "serve".to_string(),
            "acp".to_string(),
            "server.harn".to_string(),
        ]);
        assert_eq!(
            args,
            vec![
                "harn".to_string(),
                "serve".to_string(),
                "acp".to_string(),
                "server.harn".to_string(),
            ]
        );
    }

    #[test]
    fn conformance_skips_connector_clients_unless_fixture_uses_connectors() {
        let path = Path::new("conformance/tests/language/basic.harn");
        assert!(!should_install_default_connector_clients(
            "println(1)",
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
            "println(1)",
            Some(Path::new("examples/demo.harn"))
        ));
    }
}
