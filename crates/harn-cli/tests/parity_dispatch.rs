#![recursion_limit = "256"]

//! CLI parity-snapshot harness (harn#2299 / G6).
//!
//! Drives the dispatch wedge against golden fixtures so each ported
//! subcommand stays byte-for-byte compatible with the Rust handler it
//! replaces. Per-fixture format and recording flow are documented in
//! `crates/harn-cli/tests/parity_fixtures/README.md`.
//!
//! ## Today (groundwork)
//!
//! No real subcommand uses the wedge yet — see harn#2293's W1-W13. The
//! harness runs against the `cli/echo` script that ships with G1 to
//! prove the round-trip and to act as the reference for how W tickets
//! should structure their fixtures.
//!
//! ## After a W ticket lands
//!
//! Each port wave switches its Rust handler to a dispatch shim that
//! consults the `HARN_CLI_IMPL` env var (`"rust"` keeps the legacy
//! handler, `"harn"` routes through the wedge — `"harn"` becomes the
//! default once the wave is merged). The fixture harness reruns the
//! same fixture against both impls and asserts byte-identical stdout,
//! stderr, and exit code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const RECORD_ENV: &str = "HARN_CLI_PARITY_RECORD";

#[derive(Debug)]
struct Fixture {
    name: String,
    dir: PathBuf,
    command: String,
    scenario: String,
    argv: Vec<String>,
    stdin: Option<String>,
    env: BTreeMap<String, String>,
    expected_stdout: String,
    expected_stderr: String,
    expected_exit_code: i32,
}

impl Fixture {
    fn load(command_dir: &Path, scenario: &str) -> Self {
        let dir = command_dir.join(scenario);
        let command = command_dir
            .file_name()
            .and_then(|s| s.to_str())
            .expect("command dir name")
            .to_string();
        let argv = read_lines(&dir.join("argv.txt"))
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect();
        let stdin = optional_file(&dir.join("stdin.txt"));
        let env = read_env(&dir.join("env.txt"));
        let expected_stdout = read_to_string(&dir.join("stdout.txt"));
        let expected_stderr = read_to_string(&dir.join("stderr.txt"));
        let expected_exit_code = read_to_string(&dir.join("exit_code.txt"))
            .trim()
            .parse::<i32>()
            .unwrap_or_else(|_| {
                panic!("fixture {command}/{scenario}: exit_code.txt must contain a single integer")
            });
        Self {
            name: format!("{command}/{scenario}"),
            dir,
            command,
            scenario: scenario.to_string(),
            argv,
            stdin,
            env,
            expected_stdout,
            expected_stderr,
            expected_exit_code,
        }
    }

    fn assert_matches(&self, label: &str, actual: &ProcessOutcome) {
        if std::env::var_os(RECORD_ENV).is_some() {
            std::fs::write(self.dir.join("stdout.txt"), &actual.stdout).expect("write stdout.txt");
            std::fs::write(self.dir.join("stderr.txt"), &actual.stderr).expect("write stderr.txt");
            std::fs::write(
                self.dir.join("exit_code.txt"),
                format!("{}\n", actual.exit_code),
            )
            .expect("write exit_code.txt");
            return;
        }
        assert_eq!(
            actual.stdout, self.expected_stdout,
            "{label}: {} stdout mismatch\n--- expected ---\n{}\n--- actual ---\n{}\n",
            self.name, self.expected_stdout, actual.stdout
        );
        assert_eq!(
            actual.stderr, self.expected_stderr,
            "{label}: {} stderr mismatch\n--- expected ---\n{}\n--- actual ---\n{}\n",
            self.name, self.expected_stderr, actual.stderr
        );
        assert_eq!(
            actual.exit_code, self.expected_exit_code,
            "{label}: {} exit_code mismatch: expected {}, got {}",
            self.name, self.expected_exit_code, actual.exit_code
        );
    }
}

#[derive(Debug)]
struct ProcessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Invoke `harn <command> <argv>` via the real binary. Each scenario
/// runs its own subprocess so env, stdin, and signal handling stay
/// isolated.
fn run_via_harn_bin(fixture: &Fixture) -> ProcessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg(&fixture.command);
    cmd.args(&fixture.argv);
    for (k, v) in &fixture.env {
        cmd.env(k, v);
    }
    if let Some(stdin) = &fixture.stdin {
        use std::io::Write;
        use std::process::Stdio;
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn harn");
        child
            .stdin
            .as_mut()
            .expect("stdin pipe")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        let output = child.wait_with_output().expect("wait_with_output");
        ProcessOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        }
    } else {
        let output = cmd.output().expect("run harn binary");
        ProcessOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        }
    }
}

/// Invoke the dispatch wedge in-process via the public test helper.
/// Used today as the comparison point for `cli/echo` since echo has no
/// Rust handler — it's a dispatch-only demo. W tickets will replace
/// this with a real subprocess-vs-subprocess comparison toggled by
/// `HARN_CLI_IMPL`.
fn run_via_in_process_dispatch(fixture: &Fixture) -> ProcessOutcome {
    let argv = fixture.argv.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let outcome = runtime.block_on(harn_cli::dispatch::run_embedded_script(
        &fixture.command,
        argv,
        false,
    ));
    ProcessOutcome {
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        exit_code: outcome.exit_code,
    }
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn optional_file(path: &Path) -> Option<String> {
    if path.exists() {
        Some(read_to_string(path))
    } else {
        None
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    read_to_string(path)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn read_env(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in read_lines(path) {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity_fixtures")
}

#[test]
fn echo_round_trip_matches_in_process_snapshot() {
    let command_dir = fixtures_dir().join("echo");
    for scenario in &["empty", "two_args"] {
        let fixture = Fixture::load(&command_dir, scenario);
        let actual = run_via_in_process_dispatch(&fixture);
        fixture.assert_matches("in-process dispatch", &actual);
    }
}

#[test]
fn echo_subprocess_matches_snapshot() {
    // Today `harn echo` isn't wired as a subcommand — the dispatch
    // wedge lives behind the public API. This test stays as a
    // forward-looking placeholder so that when W tickets wire the
    // first real subcommand-shim, the subprocess path is already
    // exercised. Skipped at the body level until then.
    if std::env::var_os("HARN_CLI_PARITY_REQUIRE_SUBPROCESS").is_none() {
        return;
    }
    let command_dir = fixtures_dir().join("echo");
    let fixture = Fixture::load(&command_dir, "two_args");
    let actual = run_via_harn_bin(&fixture);
    fixture.assert_matches("subprocess", &actual);
}

// Silence the unused-helper warning until W tickets light up the
// subprocess path for real subcommands.
#[allow(dead_code)]
fn _force_link_run_via_harn_bin() -> ProcessOutcome {
    run_via_harn_bin(&Fixture {
        name: "_".into(),
        dir: PathBuf::new(),
        command: "version".into(),
        scenario: "_".into(),
        argv: vec![],
        stdin: None,
        env: BTreeMap::new(),
        expected_stdout: String::new(),
        expected_stderr: String::new(),
        expected_exit_code: 0,
    })
}
