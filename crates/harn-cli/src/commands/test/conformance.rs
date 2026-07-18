use super::*;

mod lint;

use lint::{format_conformance_lint_diagnostics, lint_expectation_error};

fn conformance_llm_mock_mode(harn_file: &Path) -> CliLlmMockMode {
    let fixture = harn_file.with_extension("llm-mock.jsonl");
    if fixture.is_file() {
        CliLlmMockMode::Replay {
            fixture_path: fixture,
        }
    } else {
        CliLlmMockMode::Off
    }
}

/// Testbench sidecar activation config for a single conformance test.
///
/// Sidecars are optional files adjacent to the `.harn` test:
/// - `<name>.process-tape.json` → subprocess replay tape
/// - `<name>.fs-overlay/` → filesystem overlay root
/// - `<name>.testbench-tape` → expected event tape for fidelity check
/// - `<name>.annotations.jsonl` → annotation sidecar; runner validates
///   against the emitted event tape
/// - `<name>.harness.json` → install a `Harness::null()` / `Harness::mock()`
///   test handle and assert recorded calls or deny events
///
/// When any testbench sidecar is present the runner also activates a paused
/// clock (pinned at `CONFORMANCE_TESTBENCH_START_MS`) so clock-advancing replay
/// behaves deterministically.
struct TestbenchSidecarConfig {
    process_tape: Option<PathBuf>,
    fs_overlay: Option<PathBuf>,
    expected_tape: Option<PathBuf>,
    annotations: Option<PathBuf>,
    harness: Option<PathBuf>,
}

impl TestbenchSidecarConfig {
    fn is_empty(&self) -> bool {
        self.process_tape.is_none()
            && self.fs_overlay.is_none()
            && self.expected_tape.is_none()
            && self.annotations.is_none()
    }
}

fn conformance_testbench_config(harn_file: &Path) -> TestbenchSidecarConfig {
    let process_tape = harn_file.with_extension("process-tape.json");
    let fs_overlay = harn_file.with_extension("fs-overlay");
    let expected_tape = harn_file.with_extension("testbench-tape");
    let annotations = harn_file.with_extension("annotations.jsonl");
    let harness = harn_file.with_extension("harness.json");
    TestbenchSidecarConfig {
        process_tape: process_tape.is_file().then_some(process_tape),
        fs_overlay: fs_overlay.is_dir().then_some(fs_overlay),
        expected_tape: expected_tape.is_file().then_some(expected_tape),
        annotations: annotations.is_file().then_some(annotations),
        harness: harness.is_file().then_some(harness),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HarnessSidecar {
    mode: HarnessSidecarMode,
    #[serde(default)]
    clock_at_unix_ms: Option<i64>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    fs_reads: BTreeMap<String, String>,
    #[serde(default)]
    net_gets: BTreeMap<String, String>,
    #[serde(default)]
    random_u64: Vec<u64>,
    #[serde(default)]
    expect_calls: Vec<HarnessEventExpectation>,
    #[serde(default)]
    expect_deny_events: Vec<HarnessEventExpectation>,
    #[serde(default)]
    expect_stdio: Option<String>,
    #[serde(default)]
    expect_stderr: Option<String>,
    #[serde(default)]
    stdin_lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HarnessSidecarMode {
    Null,
    Mock,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HarnessEventExpectation {
    sub_handle: String,
    method: String,
    #[serde(default)]
    args: Vec<String>,
}

impl HarnessSidecar {
    fn load(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|error| format!("read harness sidecar {}: {error}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|error| format!("parse harness sidecar {}: {error}", path.display()))
    }

    fn build_harness(&self) -> harn_vm::Harness {
        match self.mode {
            HarnessSidecarMode::Null => harn_vm::Harness::null(),
            HarnessSidecarMode::Mock => {
                let mut builder = harn_vm::Harness::mock();
                if let Some(unix_ms) = self.clock_at_unix_ms {
                    builder = builder.clock_at_unix_ms(unix_ms);
                }
                for (key, value) in &self.env {
                    builder = builder.env(key.as_str(), value.as_str());
                }
                for (path, value) in &self.fs_reads {
                    builder = builder.fs_read(path.as_str(), value.as_bytes().to_vec());
                }
                for (url, body) in &self.net_gets {
                    builder = builder.net_get(url.as_str(), body.as_str());
                }
                for value in &self.random_u64 {
                    builder = builder.random_u64(*value);
                }
                for line in &self.stdin_lines {
                    builder = builder.stdin_line(line.as_str());
                }
                builder.build()
            }
        }
    }

    fn validate(&self, harness: &harn_vm::Harness) -> Vec<String> {
        let mut errors = Vec::new();
        if !self.expect_calls.is_empty() {
            let actual = harness
                .calls()
                .into_iter()
                .map(event_from_call)
                .collect::<Vec<_>>();
            if actual != self.expect_calls {
                errors.push(format!(
                    "harness calls differed: expected {:?}, actual {:?}",
                    self.expect_calls, actual
                ));
            }
        }
        if !self.expect_deny_events.is_empty() {
            let actual = harness
                .deny_events()
                .into_iter()
                .map(event_from_deny)
                .collect::<Vec<_>>();
            if actual != self.expect_deny_events {
                errors.push(format!(
                    "harness deny events differed: expected {:?}, actual {:?}",
                    self.expect_deny_events, actual
                ));
            }
        }
        if let Some(expected) = &self.expect_stdio {
            let actual = harness.captured_stdio();
            if &actual != expected {
                errors.push(format!(
                    "harness captured stdio differed: expected {expected:?}, actual {actual:?}"
                ));
            }
        }
        if let Some(expected) = &self.expect_stderr {
            let actual = harness.captured_stderr();
            if &actual != expected {
                errors.push(format!(
                    "harness captured stderr differed: expected {expected:?}, actual {actual:?}"
                ));
            }
        }
        errors
    }
}

fn event_from_call(call: harn_vm::HarnessCall) -> HarnessEventExpectation {
    HarnessEventExpectation {
        sub_handle: harness_kind_name(call.sub_handle).to_string(),
        method: call.method,
        args: call.args,
    }
}

fn event_from_deny(event: harn_vm::DenyEvent) -> HarnessEventExpectation {
    HarnessEventExpectation {
        sub_handle: harness_kind_name(event.sub_handle).to_string(),
        method: event.method,
        args: event.args,
    }
}

fn harness_kind_name(kind: harn_vm::HarnessKind) -> &'static str {
    kind.field_name().unwrap_or("root")
}

enum ConformanceExecution {
    Completed(Result<String, ExecError>),
    TimedOut,
}

struct ConformanceRun {
    execution: ConformanceExecution,
    duration_ms: u64,
    sidecar_error: Option<String>,
}

/// Pinned testbench clock start, same constant the CLI uses, so
/// conformance fixtures and CLI invocations are interchangeable.
const CONFORMANCE_TESTBENCH_START_MS: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

async fn execute_conformance_source(
    source: &str,
    harn_file: &Path,
    timeout_ms: u64,
    llm_mock_mode: &CliLlmMockMode,
    testbench: &TestbenchSidecarConfig,
    cli_skill_dirs: &[PathBuf],
) -> Result<ConformanceRun, String> {
    use harn_vm::testbench::{
        ClockConfig, FilesystemConfig, SubprocessConfig, TapeConfig, Testbench,
    };

    harn_vm::reset_thread_local_state();
    install_cli_llm_mock_mode(llm_mock_mode)
        .map_err(|error| format!("llm mock setup error: {error}"))?;
    let state_temp_dir = tempfile::Builder::new()
        .prefix("harn-conformance-state-")
        .tempdir()
        .map_err(|error| format!("tempdir for conformance state: {error}"))?;
    let state_dir = state_temp_dir.path().join(".harn");
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("create conformance state dir: {error}"))?;
    let state_dir = state_dir.to_string_lossy().into_owned();
    let _state_dir_guard =
        ScopedEnvVar::set(harn_vm::runtime_paths::HARN_STATE_DIR_ENV, &state_dir);
    let harn_bin = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current harn executable: {error}"))?;
    let harn_bin = harn_bin.to_string_lossy().into_owned();
    let _harn_bin_guard = ScopedEnvVar::set("HARN_BIN", &harn_bin);
    let harness_sidecar = match testbench.harness.as_ref() {
        Some(path) => Some(HarnessSidecar::load(path)?),
        None => None,
    };
    let harness = harness_sidecar.as_ref().map(HarnessSidecar::build_harness);
    let harness_for_validation = harness.clone();

    // Activate testbench axes for any present sidecars. A paused clock is
    // included whenever any sidecar is active so subprocess duration_ms and
    // overlay timestamps stay deterministic.
    let tape_temp_dir = if testbench.expected_tape.is_some() || testbench.annotations.is_some() {
        Some(tempfile::tempdir().map_err(|e| format!("tempdir for tape: {e}"))?)
    } else {
        None
    };
    let tape_path = tape_temp_dir
        .as_ref()
        .map(|dir| dir.path().join("run.tape"));

    let bench = if !testbench.is_empty() {
        let clock = ClockConfig::Paused {
            starting_at_ms: CONFORMANCE_TESTBENCH_START_MS,
        };
        let subprocess = match &testbench.process_tape {
            Some(tape) => SubprocessConfig::Replay { tape: tape.clone() },
            None => SubprocessConfig::Real,
        };
        let filesystem = match &testbench.fs_overlay {
            Some(root) => FilesystemConfig::Overlay {
                worktree: root.clone(),
            },
            None => FilesystemConfig::Real,
        };
        let tape_cfg = match &tape_path {
            Some(path) => TapeConfig::Emit {
                path: path.clone(),
                argv: Vec::new(),
                script_path: Some(harn_file.to_string_lossy().into_owned()),
            },
            None => TapeConfig::Off,
        };
        Some(
            Testbench {
                clock,
                llm: harn_vm::testbench::LlmConfig::Real,
                filesystem,
                subprocess,
                network: harn_vm::testbench::NetworkConfig::Real,
                tape: tape_cfg,
            }
            .activate()
            .map_err(|e| format!("testbench activate: {e}"))?,
        )
    } else {
        None
    };

    let start = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
        match harness {
            Some(harness) => {
                execute_with_skill_dirs_and_harness(
                    source,
                    Some(harn_file),
                    cli_skill_dirs,
                    harness,
                )
                .await
            }
            None => execute_with_skill_dirs(source, Some(harn_file), cli_skill_dirs).await,
        }
    })
    .await;
    let duration_ms = start.elapsed().as_millis() as u64;
    harn_vm::llm::clear_cli_llm_mock_mode();

    // Finalize the testbench session before comparing tapes.
    if let Some(session) = bench {
        session
            .finalize()
            .map_err(|e| format!("testbench finalize: {e}"))?;
    }

    // Post-run tape fidelity check: compare emitted tape to the expected fixture.
    let mut sidecar_errors: Vec<String> = Vec::new();
    let actual_tape = match (&tape_path, &testbench.expected_tape) {
        (Some(tape_path), Some(expected_path)) => {
            use harn_vm::testbench::fidelity::{compare, FidelityMode};
            use harn_vm::testbench::tape::EventTape;
            match (EventTape::load(tape_path), EventTape::load(expected_path)) {
                (Ok(actual), Ok(expected)) => {
                    let report = compare(&expected, &actual, FidelityMode::PhaseAware);
                    if !report.is_byte_identical() {
                        sidecar_errors.push(format!(
                            "tape fidelity: {} divergence(s) vs {}",
                            report.divergences.len(),
                            expected_path.display()
                        ));
                    }
                    Some(actual)
                }
                (Err(e), _) => {
                    sidecar_errors.push(format!("load emitted tape: {e}"));
                    None
                }
                (_, Err(e)) => {
                    sidecar_errors.push(format!(
                        "load expected tape {}: {e}",
                        expected_path.display()
                    ));
                    None
                }
            }
        }
        (Some(tape_path), None) => {
            use harn_vm::testbench::tape::EventTape;
            match EventTape::load(tape_path) {
                Ok(tape) => Some(tape),
                Err(e) => {
                    sidecar_errors.push(format!("load emitted tape: {e}"));
                    None
                }
            }
        }
        _ => None,
    };
    if let (Some(annotations_path), Some(actual)) = (&testbench.annotations, actual_tape.as_ref()) {
        use harn_vm::testbench::annotations::{validate_against_tape, AnnotationTape};
        match AnnotationTape::load(annotations_path) {
            Ok(annotations) => {
                let report = validate_against_tape(&annotations, actual);
                if !report.is_ok() {
                    sidecar_errors.push(format!(
                        "annotations: {} problem(s) in {}",
                        report.problems.len(),
                        annotations_path.display()
                    ));
                }
            }
            Err(e) => sidecar_errors.push(format!(
                "load annotations {}: {e}",
                annotations_path.display()
            )),
        }
    }
    if let (Some(sidecar), Some(harness)) = (&harness_sidecar, &harness_for_validation) {
        sidecar_errors.extend(sidecar.validate(harness));
    }
    let sidecar_error: Option<String> = if sidecar_errors.is_empty() {
        None
    } else {
        Some(sidecar_errors.join("; "))
    };

    let execution = match result {
        Ok(inner_result) => ConformanceExecution::Completed(inner_result),
        Err(_) => ConformanceExecution::TimedOut,
    };
    Ok(ConformanceRun {
        execution,
        duration_ms,
        sidecar_error,
    })
}

async fn verify_unoptimized_conformance_subprocess(
    harn_file: &Path,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) -> Result<u64, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current harn executable: {error}"))?;
    let start = std::time::Instant::now();
    let mut command = tokio::process::Command::new(exe);
    harn_vm::op_interrupt::configure_tokio_kill_group(&mut command);
    let cleanup_token = harn_vm::op_interrupt::new_process_cleanup_token();
    command
        .arg("test")
        .arg("conformance")
        .arg(harn_file)
        .arg("--timeout")
        .arg(timeout_ms.to_string())
        .env(harn_vm::HARN_DISABLE_OPTIMIZATIONS_ENV, "1")
        .env(
            harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
            &cleanup_token,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for dir in cli_skill_dirs {
        command.arg("--skill-dir").arg(dir);
    }

    let wait_timeout = std::time::Duration::from_millis(timeout_ms.saturating_add(2_000));
    let mut child = command
        .spawn()
        .map_err(|error| format!("unoptimized subprocess launch failed: {error}"))?;
    let pid = child.id();
    let _cleanup_registration =
        harn_vm::op_interrupt::register_active_process_cleanup(pid, &cleanup_token, None);
    let stdout = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            terminate_unoptimized_subprocess(&mut child, pid, &cleanup_token).await;
            return Err("unoptimized subprocess stdout pipe was not captured".to_string());
        }
    };
    let stderr = match child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            terminate_unoptimized_subprocess(&mut child, pid, &cleanup_token).await;
            return Err("unoptimized subprocess stderr pipe was not captured".to_string());
        }
    };
    let stdout_task = tokio::spawn(async move {
        let mut reader = stdout;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let stderr_task = tokio::spawn(async move {
        let mut reader = stderr;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.map(|_| bytes)
    });

    let status = match tokio::time::timeout(wait_timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            terminate_unoptimized_subprocess(&mut child, pid, &cleanup_token).await;
            let _ = read_subprocess_pipe(stdout_task, "stdout").await;
            let _ = read_subprocess_pipe(stderr_task, "stderr").await;
            return Err(format!("unoptimized subprocess wait failed: {error}"));
        }
        Err(_) => {
            terminate_unoptimized_subprocess(&mut child, pid, &cleanup_token).await;
            let _ = read_subprocess_pipe(stdout_task, "stdout").await;
            let _ = read_subprocess_pipe(stderr_task, "stderr").await;
            return Err(format!(
                "unoptimized subprocess timed out after {}ms",
                wait_timeout.as_millis()
            ));
        }
    };
    let stdout = read_subprocess_pipe(stdout_task, "stdout").await?;
    let stderr = read_subprocess_pipe(stderr_task, "stderr").await?;
    let output = std::process::Output {
        status,
        stdout,
        stderr,
    };
    let duration_ms = start.elapsed().as_millis() as u64;
    if output.status.success() {
        return Ok(duration_ms);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut message = format!(
        "unoptimized subprocess exited with status {}",
        output.status
    );
    if !stdout.trim().is_empty() {
        message.push_str("\nstdout:\n");
        message.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        message.push_str("\nstderr:\n");
        message.push_str(stderr.trim_end());
    }
    Err(message)
}

async fn terminate_unoptimized_subprocess(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
    cleanup_token: &str,
) {
    if let Some(pid) = pid {
        let mut report = harn_vm::op_interrupt::signal_pid_tree_group_and_token_with_report(
            pid,
            Some(cleanup_token),
            9,
        );
        report.refresh_survivor_status();
        tracing::warn!(
            pid,
            children = report.children.len(),
            "unoptimized conformance subprocess signalled child process tree"
        );
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn read_subprocess_pipe(
    task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, String> {
    match task.await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(format!(
            "unoptimized subprocess {name} read failed: {error}"
        )),
        Err(error) => Err(format!(
            "unoptimized subprocess {name} read task failed: {error}"
        )),
    }
}

fn canonicalize_or_err(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("Failed to canonicalize {}: {error}", path.display()))
}

/// Look for `// @xfail: <reason>` in the first 50 lines of a conformance
/// test source file. Returns the reason if present.
fn read_xfail_marker(path: &Path) -> Option<String> {
    let source = fs::read_to_string(path).ok()?;
    parse_xfail_marker(&source)
}

fn parse_xfail_marker(source: &str) -> Option<String> {
    // Accept the marker in any of these comment forms within the first 50 lines:
    //   // @xfail: reason
    //   /** @xfail: reason */
    //   /**
    //    * @xfail: reason
    //    */
    // The Harn formatter sometimes converts a leading `//` comment that
    // precedes a `fn` or `pipeline` declaration into a `/** ... */` doc
    // comment, so we tolerate both shapes.
    for line in source.lines().take(50) {
        let mut s = line.trim_start();
        if let Some(rest) = s.strip_prefix("//") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("/**") {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else if let Some(rest) = s.strip_prefix('*') {
            s = rest.strip_suffix("*/").unwrap_or(rest);
        } else {
            continue;
        }
        let s = s.trim();
        if let Some(reason) = s.strip_prefix("@xfail:") {
            let r = reason.trim().trim_end_matches("*/").trim();
            if !r.is_empty() {
                return Some(r.to_string());
            }
        }
    }
    None
}

fn resolve_conformance_selection(
    suite_root: &Path,
    selection: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let suite_root = canonicalize_or_err(suite_root)?;

    let Some(selection) = selection else {
        return Ok(collect_harn_files_sorted(&suite_root));
    };

    let raw = PathBuf::from(selection);
    let mut candidates = vec![raw.clone()];
    if !raw.is_absolute() && !raw.starts_with(&suite_root) {
        candidates.push(suite_root.join(&raw));
    }

    let Some(candidate) = candidates.into_iter().find(|path| path.exists()) else {
        return Err(format!(
            "Conformance target not found: {selection}. Expected a file or directory under {}",
            suite_root.display()
        ));
    };

    let canonical = canonicalize_or_err(&candidate)?;
    if !canonical.starts_with(&suite_root) {
        return Err(format!(
            "Conformance target must be inside {}: {}",
            suite_root.display(),
            candidate.display()
        ));
    }

    if canonical.is_file() {
        if canonical.extension().is_some_and(|ext| ext == "harn") {
            return Ok(vec![canonical]);
        }
        return Err(format!(
            "Conformance target must be a .harn file or directory: {}",
            candidate.display()
        ));
    }

    let files = collect_harn_files_sorted(&canonical);
    if files.is_empty() {
        return Err(format!(
            "No .harn conformance tests found under {}",
            candidate.display()
        ));
    }
    Ok(files)
}

fn conformance_filter_matches(rel_path: &str, filter: Option<&str>) -> bool {
    let Some(pattern) = filter else {
        return true;
    };
    if let Some(re_pat) = pattern.strip_prefix("re:") {
        Regex::new(re_pat).is_ok_and(|re| re.is_match(rel_path))
    } else if pattern.contains('|') {
        pattern.split('|').any(|p| rel_path.contains(p.trim()))
    } else if pattern.contains('*') || pattern.contains('?') {
        let escaped = regex::escape(pattern)
            .replace(r"\*", ".*")
            .replace(r"\?", ".");
        Regex::new(&escaped).is_ok_and(|re| re.is_match(rel_path))
    } else {
        rel_path.contains(pattern)
    }
}

#[derive(Debug, Clone)]
struct ConformanceCaseEvaluation {
    passed: bool,
    message: Option<String>,
    diagnostic_codes: Vec<String>,
    duration_ms: u64,
}

impl ConformanceCaseEvaluation {
    fn pass_with_diagnostic_codes(diagnostic_codes: Vec<String>, duration_ms: u64) -> Self {
        Self {
            passed: true,
            message: None,
            diagnostic_codes,
            duration_ms,
        }
    }

    fn fail(message: impl Into<String>, duration_ms: u64) -> Self {
        let message = message.into();
        Self {
            passed: false,
            diagnostic_codes: extract_diagnostic_codes(&message),
            message: Some(message),
            duration_ms,
        }
    }
}

fn extract_diagnostic_codes(message: &str) -> Vec<String> {
    let re = Regex::new(r"\bHARN-[A-Z0-9]+(?:-[A-Z0-9]+)*-[0-9]{3}\b")
        .expect("diagnostic code regex compiles");
    let mut codes = BTreeSet::new();
    for capture in re.find_iter(message) {
        codes.insert(capture.as_str().to_string());
    }
    codes.into_iter().collect()
}

fn target_triple_label() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown-target"
    }
}

fn hash_file_if_present(hasher: &mut blake3::Hasher, suite_root: &Path, path: &Path) {
    if !path.is_file() {
        return;
    }
    hasher.update(b"file\0");
    let rel = path.strip_prefix(suite_root).unwrap_or(path);
    hasher.update(logical_path(rel).as_bytes());
    hasher.update(b"\0");
    match fs::read(path) {
        Ok(bytes) => hasher.update(&bytes),
        Err(error) => hasher.update(format!("read-error:{error}").as_bytes()),
    };
    hasher.update(b"\0");
}

fn hash_dir_if_present(hasher: &mut blake3::Hasher, suite_root: &Path, path: &Path) {
    if !path.is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_files_recursive(path, &mut files);
    files.sort();
    for file in files {
        hash_file_if_present(hasher, suite_root, &file);
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_files_recursive(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
}

fn conformance_snapshot_key(suite_root: &Path, selected_files: &[(PathBuf, String)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\0");
    hasher.update(target_triple_label().as_bytes());
    hasher.update(b"\0");
    match harn_vm::orchestration::current_provider_catalog_hash_blake3() {
        Ok(hash) => hasher.update(hash.as_bytes()),
        Err(error) => hasher.update(format!("provider-catalog-error:{error}").as_bytes()),
    };
    hasher.update(b"\0");

    for (harn_file, rel_path) in selected_files {
        hasher.update(b"test\0");
        hasher.update(rel_path.as_bytes());
        hasher.update(b"\0");
        hash_file_if_present(&mut hasher, suite_root, harn_file);
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("expected"),
        );
        hash_file_if_present(&mut hasher, suite_root, &harn_file.with_extension("error"));
        hash_file_if_present(&mut hasher, suite_root, &harn_file.with_extension("lint"));
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("llm-mock.jsonl"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("process-tape.json"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("testbench-tape"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("annotations.jsonl"),
        );
        hash_file_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("harness.json"),
        );
        hash_dir_if_present(
            &mut hasher,
            suite_root,
            &harn_file.with_extension("fs-overlay"),
        );
    }

    hasher.finalize().to_hex().to_string()
}

pub(crate) struct ConformanceRunOptions<'a> {
    pub(crate) verbose: bool,
    pub(crate) timing: bool,
    pub(crate) differential_optimizations: bool,
    pub(crate) json: bool,
    pub(crate) shard: Option<crate::test_runner::TestShard>,
    pub(crate) cli_skill_dirs: &'a [PathBuf],
}

fn select_conformance_shard<T>(
    items: Vec<T>,
    shard: Option<crate::test_runner::TestShard>,
) -> Vec<T> {
    let Some(shard) = shard else {
        return items;
    };
    items
        .into_iter()
        .enumerate()
        .filter_map(|(offset, item)| (offset % shard.total() == shard.index() - 1).then_some(item))
        .collect()
}

async fn evaluate_conformance_case(
    harn_file: &Path,
    expected_file: &Path,
    error_file: &Path,
    lint_file: &Path,
    rel_path: &str,
    timeout_ms: u64,
    options: &ConformanceRunOptions<'_>,
) -> ConformanceCaseEvaluation {
    if expected_file.exists() && error_file.exists() {
        return ConformanceCaseEvaluation::fail(
            format!("{rel_path}: cannot combine .expected and .error fixtures"),
            0,
        );
    }

    let source = match fs::read_to_string(harn_file) {
        Ok(source) => source,
        Err(error) => {
            return ConformanceCaseEvaluation::fail(
                format!("{rel_path}: IO error reading source: {error}"),
                0,
            );
        }
    };
    let diagnostics = match harn_parser::parse_source(&source) {
        Ok(program) => harn_lint::lint_with_source(&program, &source),
        Err(error) => {
            if lint_file.exists() || expected_file.exists() {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: parse error while linting: {error}"),
                    0,
                );
            }
            // A parser-error fixture cannot produce a lint AST. Its .error
            // assertion below remains the owning oracle for that failure.
            Vec::new()
        }
    };
    let actual_lints = format_conformance_lint_diagnostics(&diagnostics);
    let lint_codes = extract_diagnostic_codes(&actual_lints);

    if lint_file.exists() {
        let expected_lints = match fs::read_to_string(lint_file) {
            Ok(spec) => spec,
            Err(error) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading expected lint: {error}"),
                    0,
                );
            }
        };
        if let Some(error) = lint_expectation_error(&actual_lints, &expected_lints) {
            return ConformanceCaseEvaluation::fail(
                format!(
                    "{rel_path}: {error}\n  expected lint:\n    {}\n  actual lint:\n    {}",
                    expected_lints.lines().collect::<Vec<_>>().join("\n    "),
                    actual_lints.lines().collect::<Vec<_>>().join("\n    "),
                ),
                0,
            );
        }
    } else if let Some(diagnostic) = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == harn_lint::LintSeverity::Error)
    {
        return ConformanceCaseEvaluation::fail(
            format!(
                "{rel_path}: unasserted error lint; add a .lint fixture if intentional\n  {}",
                format_conformance_lint_diagnostics(std::slice::from_ref(diagnostic))
            ),
            0,
        );
    }

    if !expected_file.exists() && !error_file.exists() {
        return if lint_file.exists() {
            ConformanceCaseEvaluation::pass_with_diagnostic_codes(lint_codes, 0)
        } else {
            ConformanceCaseEvaluation::fail(
                format!("{rel_path}: missing .expected, .error, or .lint file"),
                0,
            )
        };
    }

    if expected_file.exists() {
        let expected = match fs::read_to_string(expected_file) {
            Ok(s) => normalize_expected_output(s.trim_end()),
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading expected: {e}"),
                    0,
                );
            }
        };

        let llm_mock_mode = conformance_llm_mock_mode(harn_file);
        let testbench_config = conformance_testbench_config(harn_file);
        let run = match execute_conformance_source(
            &source,
            harn_file,
            timeout_ms,
            &llm_mock_mode,
            &testbench_config,
            options.cli_skill_dirs,
        )
        .await
        {
            Ok(run) => run,
            Err(error) => {
                return ConformanceCaseEvaluation::fail(format!("{rel_path}: {error}"), 0);
            }
        };
        let duration_ms = run.duration_ms;
        if let Some(sidecar_error) = run.sidecar_error {
            return ConformanceCaseEvaluation::fail(
                format!("{rel_path}: {sidecar_error}"),
                duration_ms,
            );
        }

        return match run.execution {
            ConformanceExecution::Completed(Ok(output)) => {
                let actual = normalize_actual_output(output.trim_end());
                if actual == expected {
                    if options.differential_optimizations {
                        if let Err(error) = verify_unoptimized_conformance_subprocess(
                            harn_file,
                            timeout_ms,
                            options.cli_skill_dirs,
                        )
                        .await
                        {
                            return ConformanceCaseEvaluation::fail(
                                format!("{rel_path}: {error}"),
                                duration_ms,
                            );
                        }
                    }
                    ConformanceCaseEvaluation::pass_with_diagnostic_codes(
                        lint_codes.clone(),
                        duration_ms,
                    )
                } else {
                    let diff = simple_diff(&expected, &actual);
                    let msg = if options.verbose {
                        format!(
                            "{rel_path}:\n  expected:\n    {}\n  actual:\n    {}\n  diff:\n{diff}",
                            expected.lines().collect::<Vec<_>>().join("\n    "),
                            actual.lines().collect::<Vec<_>>().join("\n    "),
                        )
                    } else {
                        format!("{rel_path}:\n{diff}")
                    };
                    ConformanceCaseEvaluation::fail(msg, duration_ms)
                }
            }
            ConformanceExecution::Completed(Err(e)) => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: {}: {}", e.stage.label(), e.message),
                duration_ms,
            ),
            ConformanceExecution::TimedOut => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: timed out after {timeout_ms}ms"),
                timeout_ms,
            ),
        };
    }

    if error_file.exists() {
        let expected_error = match fs::read_to_string(error_file) {
            Ok(s) => s.trim_end().to_string(),
            Err(e) => {
                return ConformanceCaseEvaluation::fail(
                    format!("{rel_path}: IO error reading expected error: {e}"),
                    0,
                );
            }
        };

        let llm_mock_mode = conformance_llm_mock_mode(harn_file);
        let testbench_config = conformance_testbench_config(harn_file);
        let run = match execute_conformance_source(
            &source,
            harn_file,
            timeout_ms,
            &llm_mock_mode,
            &testbench_config,
            options.cli_skill_dirs,
        )
        .await
        {
            Ok(run) => run,
            Err(error) => {
                return ConformanceCaseEvaluation::fail(format!("{rel_path}: {error}"), 0);
            }
        };
        let duration_ms = run.duration_ms;
        if let Some(sidecar_error) = run.sidecar_error {
            return ConformanceCaseEvaluation::fail(
                format!("{rel_path}: {sidecar_error}"),
                duration_ms,
            );
        }

        return match run.execution {
            ConformanceExecution::Completed(Err(ref err))
                if error_matches(&err.message, &expected_error) =>
            {
                if options.differential_optimizations {
                    if let Err(error) = verify_unoptimized_conformance_subprocess(
                        harn_file,
                        timeout_ms,
                        options.cli_skill_dirs,
                    )
                    .await
                    {
                        return ConformanceCaseEvaluation::fail(
                            format!("{rel_path}: {error}"),
                            duration_ms,
                        );
                    }
                }
                ConformanceCaseEvaluation::pass_with_diagnostic_codes(
                    lint_codes.clone(),
                    duration_ms,
                )
            }
            ConformanceExecution::Completed(Err(err)) => ConformanceCaseEvaluation::fail(
                format!(
                    "{rel_path}:\n  expected error containing: {expected_error}\n  actual error: {err}"
                ),
                duration_ms,
            ),
            ConformanceExecution::Completed(Ok(_)) => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: expected error containing '{expected_error}', but succeeded"),
                duration_ms,
            ),
            ConformanceExecution::TimedOut => ConformanceCaseEvaluation::fail(
                format!("{rel_path}: timed out after {timeout_ms}ms"),
                timeout_ms,
            ),
        };
    }

    ConformanceCaseEvaluation::fail(
        format!("{rel_path}: missing .expected, .error, or .lint file"),
        0,
    )
}

pub(crate) async fn run_conformance_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    junit_path: Option<&str>,
    timeout_ms: u64,
    options: ConformanceRunOptions<'_>,
) {
    let show_timing = options.verbose || options.timing;
    let _disable_llm_calls = ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1");
    let _force_optimized_parent = if options.differential_optimizations {
        Some(ScopedEnvVar::unset(harn_vm::HARN_DISABLE_OPTIMIZATIONS_ENV))
    } else {
        None
    };
    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        if options.json {
            let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                CONFORMANCE_TEST_SCHEMA_VERSION,
                "conformance_directory_not_found",
                format!("Directory not found: {dir}"),
            );
            println!("{}", json_envelope::to_string_pretty(&envelope));
        } else {
            eprintln!("Directory not found: {dir}");
        }
        process::exit(1);
    }
    let suite_root = match canonicalize_or_err(&dir_path) {
        Ok(path) => path,
        Err(error) => {
            if options.json {
                let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                    CONFORMANCE_TEST_SCHEMA_VERSION,
                    "conformance_directory_error",
                    error,
                );
                println!("{}", json_envelope::to_string_pretty(&envelope));
            } else {
                eprintln!("{error}");
            }
            process::exit(1);
        }
    };

    let suite_start = std::time::Instant::now();

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut skipped_summary: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut json_results: Vec<ConformanceJsonResult> = Vec::new();
    let mut json_summary = ConformanceJsonSummary::default();
    let mut report = TestReport::new("conformance", Some(&suite_root));

    let harn_files = match resolve_conformance_selection(&suite_root, selection) {
        Ok(files) => files,
        Err(error) => {
            if options.json {
                let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
                    CONFORMANCE_TEST_SCHEMA_VERSION,
                    "conformance_selection_error",
                    error,
                );
                println!("{}", json_envelope::to_string_pretty(&envelope));
            } else {
                eprintln!("{error}");
            }
            process::exit(1);
        }
    };

    let selected_harn_files: Vec<(PathBuf, String)> = harn_files
        .into_iter()
        .filter_map(|harn_file| {
            let rel_path = harn_file.strip_prefix(&suite_root).unwrap_or(&harn_file);
            let rel_path = logical_path(rel_path);
            conformance_filter_matches(&rel_path, filter).then_some((harn_file, rel_path))
        })
        .collect();
    let selected_harn_files = select_conformance_shard(selected_harn_files, options.shard);

    for (harn_file, rel_path) in &selected_harn_files {
        let expected_file = harn_file.with_extension("expected");
        let error_file = harn_file.with_extension("error");
        let lint_file = harn_file.with_extension("lint");

        // Honor `// @xfail: <reason>` markers in the first 50 lines of a
        // conformance test. Text mode preserves the historical skip behavior.
        // JSON mode executes the test so stale markers become
        // `xfail_unexpected_pass` failures that force marker cleanup.
        let xfail_reason = read_xfail_marker(harn_file);
        if !options.json {
            if let Some(reason) = xfail_reason.as_ref() {
                println!("  \x1b[33mSKIP\x1b[0m  {rel_path}  ({reason})");
                skipped_summary.push((rel_path.clone(), reason.clone()));
                skipped += 1;
                continue;
            }
        }

        if !expected_file.exists() && !error_file.exists() && !lint_file.exists() {
            continue;
        }

        let evaluation = evaluate_conformance_case(
            harn_file,
            &expected_file,
            &error_file,
            &lint_file,
            rel_path,
            timeout_ms,
            &options,
        )
        .await;

        if options.json {
            let outcome = match (&xfail_reason, evaluation.passed) {
                (Some(_), true) => ConformanceJsonOutcome::XfailUnexpectedPass,
                (Some(_), false) => ConformanceJsonOutcome::XfailExpected,
                (None, true) => ConformanceJsonOutcome::Pass,
                (None, false) => ConformanceJsonOutcome::Fail,
            };
            json_summary.record(outcome);

            let message = match (
                outcome,
                xfail_reason.as_deref(),
                evaluation.message.as_deref(),
            ) {
                (ConformanceJsonOutcome::XfailUnexpectedPass, Some(reason), _) => {
                    Some(format!("xfail marker is stale: {reason}"))
                }
                (ConformanceJsonOutcome::XfailExpected, Some(reason), Some(message)) => {
                    Some(format!("expected failure ({reason}): {message}"))
                }
                (ConformanceJsonOutcome::XfailExpected, Some(reason), None) => {
                    Some(format!("expected failure ({reason})"))
                }
                (_, _, Some(message)) => Some(message.to_string()),
                _ => None,
            };

            let junit_passed = matches!(
                outcome,
                ConformanceJsonOutcome::Pass | ConformanceJsonOutcome::XfailExpected
            );
            report.push(TestCaseReport {
                name: rel_path.clone(),
                file: rel_path.clone(),
                classname: rel_path.clone(),
                outcome: if junit_passed {
                    TestOutcome::Passed
                } else {
                    TestOutcome::Failed
                },
                duration_ms: evaluation.duration_ms,
                timeout: None,
                phases: None,
                message: if junit_passed { None } else { message.clone() },
                captured_output: None,
            });
            json_results.push(ConformanceJsonResult {
                name: rel_path.clone(),
                outcome,
                duration_ms: evaluation.duration_ms,
                message,
                diagnostic_codes: evaluation.diagnostic_codes,
            });
            continue;
        }

        if evaluation.passed {
            if show_timing {
                println!(
                    "  \x1b[32mPASS\x1b[0m  {rel_path} ({} ms)",
                    evaluation.duration_ms
                );
            } else {
                println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
            }
            report.push(TestCaseReport {
                name: rel_path.clone(),
                file: rel_path.clone(),
                classname: rel_path.clone(),
                outcome: TestOutcome::Passed,
                duration_ms: evaluation.duration_ms,
                timeout: None,
                phases: None,
                message: None,
                captured_output: None,
            });
            passed += 1;
        } else {
            if show_timing {
                println!(
                    "  \x1b[31mFAIL\x1b[0m  {rel_path} ({} ms)",
                    evaluation.duration_ms
                );
            } else {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
            }
            let msg = evaluation
                .message
                .unwrap_or_else(|| format!("{rel_path}: failed without diagnostic message"));
            errors.push(msg.clone());
            report.push(TestCaseReport {
                name: rel_path.clone(),
                file: rel_path.clone(),
                classname: rel_path.clone(),
                outcome: TestOutcome::Failed,
                duration_ms: evaluation.duration_ms,
                timeout: None,
                phases: None,
                message: Some(msg),
                captured_output: None,
            });
            failed += 1;
        }
    }

    let total_duration_ms = suite_start.elapsed().as_millis() as u64;
    report.set_duration_ms(total_duration_ms);

    if options.json {
        if let Some(path) = junit_path {
            write_junit_xml_or_exit(path, &report, false);
        }
        let snapshot_key = conformance_snapshot_key(&suite_root, &selected_harn_files);
        let ok = json_summary.is_success();
        let error = (!ok).then(|| JsonError {
            code: "conformance_failed".to_string(),
            message: "one or more conformance tests failed or unexpectedly passed an xfail marker"
                .to_string(),
            details: serde_json::json!({
                "fail": json_summary.fail,
                "xfail_unexpected_pass": json_summary.xfail_unexpected_pass,
            }),
        });
        let envelope = JsonEnvelope {
            schema_version: CONFORMANCE_TEST_SCHEMA_VERSION,
            ok,
            data: Some(ConformanceJsonReport::new(
                snapshot_key,
                json_results,
                json_summary,
            )),
            error,
            warnings: Vec::new(),
        };
        println!("{}", json_envelope::to_string_pretty(&envelope));
        if !ok {
            process::exit(1);
        }
        return;
    }

    println!();
    let total = passed + failed + skipped;
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    } else {
        println!(
            "\x1b[32m{passed} passed, {failed} failed, {skipped} skipped, {total} total\x1b[0m"
        );
    }

    if !skipped_summary.is_empty() {
        println!();
        println!("Skipped (xfail):");
        for (path, reason) in &skipped_summary {
            println!("  {path}  ({reason})");
        }
    }

    if show_timing {
        println!();
        println!("Total time: {total_duration_ms} ms");

        print_per_test_timing(&report);
    }

    if let Some(path) = junit_path {
        write_junit_xml_or_exit(path, &report, true);
    }

    if !errors.is_empty() {
        println!();
        println!("Failures:");
        for err in &errors {
            println!("  {err}");
        }
        process::exit(1);
    }
}

pub(crate) async fn run_conformance_determinism_tests(
    dir: &str,
    selection: Option<&str>,
    filter: Option<&str>,
    timeout_ms: u64,
    cli_skill_dirs: &[PathBuf],
) {
    let dir_path = PathBuf::from(dir);
    let suite_root = canonicalize_or_err(&dir_path).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let files = resolve_conformance_selection(&suite_root, selection).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = Vec::new();

    for path in files {
        let rel_path = path.strip_prefix(&suite_root).unwrap_or(&path);
        let rel_path = logical_path(rel_path);
        if let Some(pattern) = filter {
            let matched = if let Some(re_pat) = pattern.strip_prefix("re:") {
                Regex::new(re_pat).is_ok_and(|re| re.is_match(&rel_path))
            } else {
                rel_path.contains(pattern)
            };
            if !matched {
                continue;
            }
        }
        match run_determinism_case(&path, timeout_ms, cli_skill_dirs).await {
            Ok(()) => {
                println!("  \x1b[32mPASS\x1b[0m  {rel_path}");
                passed += 1;
            }
            Err(error) => {
                println!("  \x1b[31mFAIL\x1b[0m  {rel_path}");
                failed += 1;
                errors.push(error);
            }
        }
    }

    println!();
    if failed > 0 {
        println!(
            "\x1b[31m{passed} passed, {failed} failed, {} total\x1b[0m",
            passed + failed
        );
        println!();
        println!("Failures:");
        for error in errors {
            println!("  {error}");
        }
        process::exit(1);
    }
    println!(
        "\x1b[32m{passed} passed, {failed} failed, {} total\x1b[0m",
        passed + failed
    );
}

#[cfg(test)]
mod tests;
