use super::*;

pub(super) fn run_eval_pack_command(
    spec: &EvalPackCommandSpec,
    default_cwd: &Path,
    stdin_payload: Option<&serde_json::Value>,
    default_timeout_seconds: f64,
) -> Result<EvalPackCommandOutput, VmError> {
    let timeout_seconds = command_timeout(spec).unwrap_or(default_timeout_seconds);
    let timeout = timeout_seconds
        .is_finite()
        .then_some(timeout_seconds)
        .filter(|seconds| *seconds > 0.0)
        .ok_or_else(|| {
            VmError::Runtime(
                "eval pack command timeout must be a positive finite number of seconds".to_string(),
            )
        })?;
    let mut command = eval_pack_command(spec)?;
    command
        .current_dir(command_cwd(spec, default_cwd))
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_command_env(spec, &mut command);

    let started = crate::clock_mock::leak_audit::instant_now("eval_pack.command.started");
    let mut child = command
        .spawn()
        .map_err(|e| VmError::Runtime(format!("eval pack command spawn failed: {e}")))?;
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    });

    let mut stdin_error = None;
    if let Some(payload) = stdin_payload {
        match child.stdin.take() {
            Some(mut stdin) => {
                if let Err(error) = serde_json::to_writer(&mut stdin, payload) {
                    stdin_error = Some(format!("eval pack command stdin encode failed: {error}"));
                } else if let Err(error) = stdin.write_all(b"\n") {
                    stdin_error = Some(format!("eval pack command stdin write failed: {error}"));
                }
            }
            None => {
                stdin_error = Some("eval pack command stdin pipe was unavailable".to_string());
            }
        }
    }

    let timeout = Duration::from_secs_f64(timeout);
    let status = if stdin_error.is_some() {
        let _ = child.kill();
        let _ = child.wait();
        None
    } else {
        match child
            .wait_timeout(timeout)
            .map_err(|e| VmError::Runtime(format!("eval pack command wait failed: {e}")))?
        {
            Some(status) => Some(status),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    };
    if let Some(error) = stdin_error {
        let _ = join_command_reader(stdout_reader, "stdout")?;
        let _ = join_command_reader(stderr_reader, "stderr")?;
        return Err(VmError::Runtime(error));
    }
    let wall_time_seconds = started.elapsed().as_secs_f64();
    let stdout = join_command_reader(stdout_reader, "stdout")?;
    let stderr = join_command_reader(stderr_reader, "stderr")?;
    let timed_out = status.is_none();
    let exit_code = status.and_then(|status| status.code()).unwrap_or(-1) as i64;
    Ok(EvalPackCommandOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
        wall_time_seconds,
    })
}

fn eval_pack_command(spec: &EvalPackCommandSpec) -> Result<Command, VmError> {
    match spec {
        EvalPackCommandSpec::Shell(command) => shell_command(command),
        EvalPackCommandSpec::Argv(argv) => argv_command(argv),
        EvalPackCommandSpec::Object(object) => {
            if let Some(command) = object.command.as_deref() {
                shell_command(command)
            } else {
                argv_command(&object.argv)
            }
        }
    }
}

fn shell_command(command: &str) -> Result<Command, VmError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(VmError::Runtime(
            "eval pack shell command must not be empty".to_string(),
        ));
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        Ok(cmd)
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        Ok(cmd)
    }
}

fn argv_command(argv: &[String]) -> Result<Command, VmError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(VmError::Runtime(
            "eval pack argv command must include a program".to_string(),
        ));
    };
    if program.trim().is_empty() {
        return Err(VmError::Runtime(
            "eval pack argv command program must not be empty".to_string(),
        ));
    }
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

fn command_cwd(spec: &EvalPackCommandSpec, default_cwd: &Path) -> PathBuf {
    let cwd = match spec {
        EvalPackCommandSpec::Object(EvalPackCommandObject { cwd: Some(cwd), .. }) => cwd.as_str(),
        _ => return default_cwd.to_path_buf(),
    };
    let path = PathBuf::from(cwd);
    if path.is_absolute() {
        path
    } else {
        default_cwd.join(path)
    }
}

fn apply_command_env(spec: &EvalPackCommandSpec, command: &mut Command) {
    if let EvalPackCommandSpec::Object(object) = spec {
        command.envs(&object.env);
    }
}

fn command_timeout(spec: &EvalPackCommandSpec) -> Option<f64> {
    match spec {
        EvalPackCommandSpec::Object(object) => object.timeout_seconds,
        _ => None,
    }
}

fn join_command_reader(
    reader: Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stream: &str,
) -> Result<String, VmError> {
    let Some(reader) = reader else {
        return Ok(String::new());
    };
    let bytes = reader
        .join()
        .map_err(|_| VmError::Runtime(format!("eval pack command {stream} reader panicked")))?
        .map_err(|e| VmError::Runtime(format!("eval pack command {stream} read failed: {e}")))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub(super) fn eval_pack_live_workspace(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
) -> Result<PathBuf, VmError> {
    let workspace = case
        .workspace
        .as_deref()
        .or(case.project.as_deref())
        .ok_or_else(|| {
            VmError::Runtime("eval pack live-verify case is missing workspace".to_string())
        })?;
    let workspace = resolve_manifest_path(base_dir, workspace);
    if !workspace.is_dir() {
        return Err(VmError::Runtime(format!(
            "eval pack live-verify workspace does not exist: {}",
            workspace.display()
        )));
    }
    Ok(workspace)
}

pub(super) fn eval_pack_live_executor_request(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    case_id: &str,
    trial: usize,
    trial_count: usize,
    workspace: &Path,
    base_dir: Option<&Path>,
) -> Result<serde_json::Value, VmError> {
    Ok(serde_json::json!({
        "schema": LIVE_EXECUTOR_REQUEST_SCHEMA,
        "manifest": {
            "id": &manifest.id,
            "base_dir": base_dir.map(|path| path.display().to_string()),
            "metadata": &manifest.metadata,
        },
        "case": {
            "id": case_id,
            "name": &case.name,
            "task": &case.task,
            "workspace": workspace.display().to_string(),
            "project": &case.project,
            "verify_command": command_spec_json(case.verify_command.as_ref())?,
            "expected_output_paths": &case.expected_output_paths,
            "required_output_snippets": &case.required_output_snippets,
            "tool_budgets": &case.tool_budgets,
            "metadata": &case.metadata,
            "case_fingerprint": &case.case_fingerprint,
        },
        "trial": trial,
        "trials": trial_count,
    }))
}

fn command_spec_json(spec: Option<&EvalPackCommandSpec>) -> Result<serde_json::Value, VmError> {
    match spec {
        Some(spec) => serde_json::to_value(spec)
            .map_err(|e| VmError::Runtime(format!("eval pack command encode failed: {e}"))),
        None => Ok(serde_json::Value::Null),
    }
}

pub(super) fn live_outcome_from_executor_output(
    output: EvalPackCommandOutput,
    failures: &mut Vec<String>,
) -> EvalPackLiveVerifyOutcome {
    let mut outcome = parse_live_outcome_stdout(&output.stdout).unwrap_or_else(|error| {
        if !output.stdout.trim().is_empty() {
            failures.push(error);
        }
        EvalPackLiveVerifyOutcome::default()
    });
    if output.timed_out {
        outcome.timed_out = true;
    }
    if output.exit_code != 0 {
        failures.push(format!(
            "live executor exited {}{}",
            output.exit_code,
            command_failure_excerpt(&output)
        ));
    }
    if outcome.wall_time_seconds == 0.0 {
        outcome.wall_time_seconds = output.wall_time_seconds;
    }
    outcome
}

fn parse_live_outcome_stdout(stdout: &str) -> Result<EvalPackLiveVerifyOutcome, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(EvalPackLiveVerifyOutcome::default());
    }
    serde_json::from_str(trimmed)
        .or_else(|_| {
            trimmed
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .ok_or_else(|| serde_json::Error::io(std::io::ErrorKind::UnexpectedEof.into()))
                .and_then(|line| serde_json::from_str(line.trim()))
        })
        .map_err(|error| format!("live executor stdout did not contain a JSON outcome: {error}"))
}

pub(super) fn live_outcome_verification(outcome: &EvalPackLiveVerifyOutcome) -> String {
    if let Some(verification) = outcome.verification.as_deref() {
        return normalize_live_verification(verification);
    }
    if outcome.timed_out {
        return "FAIL".to_string();
    }
    if let Some(exit_code) = outcome.verification_exit_code {
        return if exit_code == 0 { "PASS" } else { "FAIL" }.to_string();
    }
    if let Some(passed) = outcome.passed {
        return if passed { "PASS" } else { "FAIL" }.to_string();
    }
    "PASS".to_string()
}

fn normalize_live_verification(verification: &str) -> String {
    match verification.trim().to_ascii_lowercase().as_str() {
        "pass" | "passed" | "success" | "ok" => "PASS".to_string(),
        "skip" | "skipped" => "skip".to_string(),
        _ => "FAIL".to_string(),
    }
}

pub(super) fn command_failure_excerpt(output: &EvalPackCommandOutput) -> String {
    let stderr = compact_output_excerpt(&output.stderr);
    if !stderr.is_empty() {
        return format!("; stderr: {stderr}");
    }
    let stdout = compact_output_excerpt(&output.stdout);
    if stdout.is_empty() {
        String::new()
    } else {
        format!("; stdout: {stdout}")
    }
}

fn compact_output_excerpt(output: &str) -> String {
    let compact = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let max_chars = 240;
    if compact.chars().count() > max_chars {
        format!("{}...", compact.chars().take(max_chars).collect::<String>())
    } else {
        compact
    }
}

pub(super) fn normalized_live_produced_paths(
    case: &EvalPackCase,
    outcome: &EvalPackLiveVerifyOutcome,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for path in outcome
        .produced_paths
        .iter()
        .chain(case.expected_output_paths.iter())
    {
        if !path.trim().is_empty() && seen.insert(path.clone()) {
            paths.push(path.clone());
        }
    }
    paths
}

pub(super) fn eval_pack_live_expected_path_failures(
    workspace: &Path,
    paths: &[String],
) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| {
            let resolved = resolve_manifest_path(Some(workspace), path);
            (!resolved.exists()).then(|| {
                format!(
                    "expected output path does not exist: {}",
                    resolved.display()
                )
            })
        })
        .collect()
}

pub(super) fn eval_pack_live_required_snippet_failures(
    workspace: &Path,
    paths: &[String],
    snippets: &[String],
) -> Vec<String> {
    let readable_outputs = paths
        .iter()
        .map(|path| resolve_manifest_path(Some(workspace), path))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    snippets
        .iter()
        .filter(|snippet| !snippet.is_empty())
        .filter_map(|snippet| {
            let found = readable_outputs.iter().any(|path| {
                std::fs::read_to_string(path)
                    .map(|content| content.contains(snippet))
                    .unwrap_or(false)
            });
            (!found).then(|| format!("required output snippet not found: {snippet:?}"))
        })
        .collect()
}

pub(super) fn eval_pack_live_tool_budget_failures(
    budgets: &BTreeMap<String, usize>,
    summary: &serde_json::Value,
) -> Vec<String> {
    budgets
        .iter()
        .filter_map(|(name, limit)| {
            let count = live_tool_summary_count(summary, name)?;
            (count > *limit)
                .then(|| format!("tool budget {name} exceeded: {count} calls > {limit}"))
        })
        .collect()
}

pub(super) fn live_tool_summary_count(summary: &serde_json::Value, name: &str) -> Option<usize> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return None;
    }
    if normalized == "total" {
        return json_usize_from_keys(summary, &["total", "calls", "tool_calls", "toolCalls"]);
    }
    json_usize_from_keys(summary, &[normalized])
        .or_else(|| {
            summary
                .get("by_tool")
                .or_else(|| summary.get("byTool"))
                .and_then(|value| json_usize_from_keys(value, &[normalized]))
        })
        .or_else(|| {
            summary
                .get("tools")
                .and_then(|value| json_usize_from_keys(value, &[normalized]))
        })
        .or_else(|| {
            // Fall back to counting occurrences in the per-call `sequence`
            // array. The in-process coding-agent executor only emits
            // `{total, rejected, sequence, successful}` (no `by_tool` map), so
            // without this a named per-tool budget like `{edit: 1}` would
            // silently never be enforced for live coding-agent evals.
            summary
                .get("sequence")
                .and_then(serde_json::Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter(|call| call.as_str() == Some(normalized))
                        .count()
                })
        })
}

fn json_usize_from_keys(value: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(json_value_usize)
}

fn json_value_usize(value: &serde_json::Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| value.as_i64().and_then(|value| usize::try_from(value).ok()))
}
