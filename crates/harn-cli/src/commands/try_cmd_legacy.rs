use crate::cli::TryArgs;

/// Host-side synth-into-tempfile-then-`execute_run` fallback retained for
/// parity snapshot coverage.
pub(super) async fn run(args: TryArgs, provider: &str, model: &str) {
    use std::collections::HashSet;
    use std::fs;

    use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};

    let escaped = harn_lexer::escape_string_literal(&args.prompt);
    let max_iters = args.max_iterations;
    let provider = harn_lexer::escape_string_literal(provider);
    let model = harn_lexer::escape_string_literal(model);
    let tool_format_line = args
        .tool_format
        .as_deref()
        .map(|value| {
            format!(
                "    tool_format: \"{}\",\n",
                harn_lexer::escape_string_literal(value)
            )
        })
        .unwrap_or_default();
    let override_reason_line = args
        .override_reason
        .as_deref()
        .map(|value| {
            format!(
                "    tool_format_override_reason: \"{}\",\n",
                harn_lexer::escape_string_literal(value)
            )
        })
        .unwrap_or_default();
    let script = format!(
        "let result = agent_loop(\"{escaped}\", nil, {{\n    max_iterations: {max_iters},\n    llm_retries: 2,\n    provider: \"{provider}\",\n    model: \"{model}\",\n{tool_format_line}{override_reason_line}}})\nlet warning = agent_first_tool_format_override_warning_text(transcript_events(result?.transcript))\nif warning != nil {{\n    __io_eprintln(warning)\n}}\n__io_println(result.text)\n"
    );

    let tmp = match tempfile::Builder::new()
        .prefix("harn-try-")
        .suffix(".harn")
        .tempfile()
    {
        Ok(t) => t,
        Err(error) => {
            eprintln!("failed to create temp file: {error}");
            std::process::exit(1);
        }
    };
    let path = tmp.path().to_path_buf();
    let wrapped = format!(
        "import {{ agent_first_tool_format_override_warning_text }} from \"std/agent/options\"\n\npipeline main(task) {{\n{script}}}\n"
    );
    if let Err(error) = fs::write(&path, &wrapped) {
        eprintln!("failed to write temp file: {error}");
        std::process::exit(1);
    }

    let outcome: RunOutcome = execute_run(
        &path.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }

    drop(tmp);
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code);
    }
}
