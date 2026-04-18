use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn write_file(dir: &Path, relative: &str, contents: &str) -> String {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

fn run_harn(temp: &TempDir, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harn"));
    command.current_dir(temp.path());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn llm_mock_replays_fifo_fixtures_for_non_mock_provider() {
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "script.harn",
        r#"
pipeline default() {
  println(llm_call("same prompt", nil, {provider: env_or("TEST_PROVIDER", "mock")}).text)
  println(llm_call("same prompt", nil, {provider: env_or("TEST_PROVIDER", "mock")}).text)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"text":"first","model":"fixture-model"}
{"text":"second","model":"fixture-model"}
"#,
    );

    let output = run_harn(
        &temp,
        &["run", "--llm-mock", &fixtures, &script],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "first\nsecond\n");
}

#[test]
fn llm_mock_reuses_glob_matches() {
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "script.harn",
        r#"
pipeline default() {
  println(llm_call("say hello please", nil, {provider: env_or("TEST_PROVIDER", "mock")}).text)
  println(llm_call("say hello again", nil, {provider: env_or("TEST_PROVIDER", "mock")}).text)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"match":"*hello*","text":"matched","model":"fixture-model"}
"#,
    );

    let output = run_harn(
        &temp,
        &["run", "--llm-mock", &fixtures, &script],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "matched\nmatched\n");
}

#[test]
fn llm_mock_reports_unmatched_prompt_snippet() {
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "script.harn",
        r#"
pipeline default() {
  println(llm_call("this prompt is intentionally unmatched for fixture coverage", nil, {provider: env_or("TEST_PROVIDER", "mock")}).text)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"match":"*different*","text":"nope","model":"fixture-model"}
"#,
    );

    let output = run_harn(
        &temp,
        &["run", "--llm-mock", &fixtures, &script],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(!output.status.success(), "stdout={}", stdout(&output));
    let stderr = stderr(&output);
    assert!(stderr.contains("No --llm-mock fixture matched prompt:"));
    assert!(stderr.contains("this prompt is intentionally unmatched"));
}

#[test]
fn llm_mock_record_replays_identical_output() {
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "script.harn",
        r#"
pipeline default() {
  let provider = env_or("TEST_PROVIDER", "mock")
  let result = llm_call("hello world", nil, {provider: provider})
  println(transcript_render_full(result.transcript))
}
"#,
    );
    let fixtures = temp.path().join("recorded.jsonl");

    let recorded = run_harn(
        &temp,
        &[
            "run",
            "--llm-mock-record",
            &fixtures.to_string_lossy(),
            &script,
        ],
        &[("TEST_PROVIDER", "mock")],
    );
    assert!(
        recorded.status.success(),
        "status={:?}\nstderr={}",
        recorded.status.code(),
        stderr(&recorded)
    );

    let recorded_fixture = fs::read_to_string(&fixtures).unwrap();
    assert_eq!(recorded_fixture.lines().count(), 1);

    let replayed = run_harn(
        &temp,
        &["run", "--llm-mock", &fixtures.to_string_lossy(), &script],
        &[("TEST_PROVIDER", "anthropic")],
    );
    assert!(
        replayed.status.success(),
        "status={:?}\nstderr={}",
        replayed.status.code(),
        stderr(&replayed)
    );

    assert_eq!(stdout(&recorded), stdout(&replayed));
}
