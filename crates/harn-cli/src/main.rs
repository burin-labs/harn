mod a2a;
mod acp;
mod cli;
mod commands;
mod config;
mod package;
mod test_runner;

use clap::{error::ErrorKind, CommandFactory, Parser as ClapParser};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

use cli::{Cli, Command, RunsCommand};
use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

#[tokio::main]
async fn main() {
    let raw_args: Vec<String> = env::args().collect();
    if raw_args.len() == 2 && raw_args[1].ends_with(".harn") {
        commands::run::run_file(
            &raw_args[1],
            false,
            std::collections::HashSet::new(),
            Vec::new(),
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
        Command::Run(args) => {
            let denied =
                commands::run::build_denied_builtins(args.deny.as_deref(), args.allow.as_deref());

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
                    commands::run::run_file(&tmp_str, args.trace, denied, args.argv.clone()).await;
                    drop(tmp);
                }
                (None, Some(file)) => {
                    commands::run::run_file(file, args.trace, denied, args.argv.clone()).await
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
            let cross_file_imports = commands::check::collect_cross_file_imports(&files);
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
                let outcome = commands::check::check_file_inner(file, &config, &cross_file_imports);
                should_fail |= outcome.should_fail(config.strict);
            }
            if should_fail {
                process::exit(1);
            }
        }
        Command::Contracts(args) => {
            commands::contracts::handle_contracts_command(args).await;
        }
        Command::Lint(args) => {
            let targets: Vec<&str> = args.targets.iter().map(String::as_str).collect();
            let files = commands::check::collect_harn_targets(&targets);
            if files.is_empty() {
                command_error("no .harn files found under the given target(s)");
            }
            let cross_file_imports = commands::check::collect_cross_file_imports(&files);
            if args.fix {
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    commands::check::apply_harn_lint_config(file, &mut config);
                    let require_header = args.require_file_header
                        || commands::check::harn_lint_require_file_header(file);
                    commands::check::lint_fix_file(
                        file,
                        &config,
                        &cross_file_imports,
                        require_header,
                    );
                }
            } else {
                let mut should_fail = false;
                for file in &files {
                    let mut config = package::load_check_config(Some(file));
                    commands::check::apply_harn_lint_config(file, &mut config);
                    let require_header = args.require_file_header
                        || commands::check::harn_lint_require_file_header(file);
                    let outcome = commands::check::lint_file_inner(
                        file,
                        &config,
                        &cross_file_imports,
                        require_header,
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
            commands::check::fmt_targets(&targets, args.check, &opts);
        }
        Command::Test(args) => {
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
        Command::Init(args) | Command::New(args) => {
            commands::init::init_project(args.name.as_deref(), args.template)
        }
        Command::Doctor(args) => commands::doctor::run_doctor(!args.no_network).await,
        Command::Serve(args) => a2a::run_a2a_server(&args.file, args.port).await,
        Command::Acp(args) => acp::run_acp_server(args.pipeline.as_deref()).await,
        Command::McpServe(args) => commands::run::run_file_mcp_serve(&args.file).await,
        Command::Mcp(args) => commands::mcp::handle_mcp_command(&args.command).await,
        Command::Watch(args) => {
            let denied =
                commands::run::build_denied_builtins(args.deny.as_deref(), args.allow.as_deref());
            commands::run::run_watch(&args.file, denied).await;
        }
        Command::Portal(args) => {
            commands::portal::run_portal(&args.dir, &args.host, args.port, args.open).await
        }
        Command::Runs(args) => match args.command {
            RunsCommand::Inspect(inspect) => {
                inspect_run_record(&inspect.path, inspect.compare.as_deref())
            }
        },
        Command::Replay(args) => replay_run_record(&args.path),
        Command::Eval(args) => eval_run_record(&args.path, args.compare.as_deref()),
        Command::Repl => commands::repl::run_repl().await,
        Command::Bench(args) => commands::bench::run_bench(&args.file, args.iterations).await,
        Command::Viz(args) => commands::viz::run_viz(&args.file, args.output.as_deref()),
        Command::Install => package::install_packages(),
        Command::Add(args) => package::add_package(
            &args.name,
            args.git.as_deref(),
            args.tag.as_deref(),
            args.path.as_deref(),
        ),
        Command::ModelInfo(args) => print_model_info(&args.model).await,
        Command::DumpHighlightKeywords(args) => {
            commands::dump_highlight_keywords::run(&args.output, args.check);
        }
    }
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

async fn print_model_info(model: &str) {
    let (resolved_id, resolved_provider) = harn_vm::llm_config::resolve_model(model);
    let provider =
        resolved_provider.unwrap_or_else(|| harn_vm::llm_config::infer_provider(&resolved_id));
    let api_key_result = harn_vm::llm::resolve_api_key(&provider);
    let api_key_set = api_key_result.is_ok();
    let api_key = api_key_result.unwrap_or_default();
    let tool_format = harn_vm::llm_config::default_tool_format(&resolved_id, &provider);
    let context_window =
        harn_vm::llm::fetch_provider_max_context(&provider, &resolved_id, &api_key).await;
    let payload = serde_json::json!({
        "alias": model,
        "id": resolved_id,
        "provider": provider,
        "tool_format": tool_format,
        "api_key_set": api_key_set,
        "context_window": context_window,
    });
    println!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|error| {
            command_error(&format!("failed to serialize model info: {error}"))
        })
    );
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
    let content = fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("Failed to read eval manifest {}: {error}", path.display());
        process::exit(1);
    });
    let mut manifest: harn_vm::orchestration::EvalSuiteManifest = serde_json::from_str(&content)
        .unwrap_or_else(|error| {
            eprintln!("Failed to parse eval manifest {}: {error}", path.display());
            process::exit(1);
        });
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    manifest
}

fn file_looks_like_eval_manifest(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("_type").and_then(|value| value.as_str()) == Some("eval_suite_manifest")
        || json.get("cases").is_some()
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
    println!("Transition delta: {}", diff.transition_count_delta);
    println!("Artifact delta: {}", diff.artifact_count_delta);
    println!("Checkpoint delta: {}", diff.checkpoint_count_delta);
    for stage in &diff.stage_diffs {
        println!("- {} [{}]", stage.node_id, stage.change);
        for detail in &stage.details {
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

fn eval_run_record(path: &str, compare: Option<&str>) {
    let path_buf = PathBuf::from(path);
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
        Err(_) => {
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

    let type_diagnostics = TypeChecker::new().check(&program);
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
            let execution_result = vm.execute(&chunk).await.map_err(|e| e.to_string());
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
