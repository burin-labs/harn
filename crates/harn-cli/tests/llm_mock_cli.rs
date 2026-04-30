mod test_util;

use std::fs;
use std::path::Path;
use std::process::Output;

use tempfile::TempDir;
use test_util::process::harn_command;

fn write_file(dir: &Path, relative: &str, contents: &str) -> String {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

fn run_harn(temp: &TempDir, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = harn_command();
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

#[test]
fn playground_llm_mock_replays_fifo_fixtures_for_non_mock_provider() {
    let temp = TempDir::new().unwrap();
    let host = write_file(
        temp.path(),
        "host.harn",
        r#"
pub fn build_prompt(task) {
  return "playground prompt: " + task
}
"#,
    );
    let script = write_file(
        temp.path(),
        "pipeline.harn",
        r#"
pipeline default() {
  let result = llm_call(build_prompt(env_or("HARN_TASK", "")), nil, {
    provider: env_or("TEST_PROVIDER", "mock"),
  })
  println(result.text)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"text":"playground replay","model":"fixture-model"}
"#,
    );

    let output = run_harn(
        &temp,
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "demo",
            "--llm-mock",
            &fixtures,
        ],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "playground replay\n");
}

#[test]
fn playground_llm_mock_record_replays_identical_output() {
    let temp = TempDir::new().unwrap();
    let host = write_file(
        temp.path(),
        "host.harn",
        r#"
pub fn build_prompt(task) {
  return "playground prompt: " + task
}
"#,
    );
    let script = write_file(
        temp.path(),
        "pipeline.harn",
        r#"
pipeline default() {
  let provider = env_or("TEST_PROVIDER", "mock")
  let result = llm_call(build_prompt(env_or("HARN_TASK", "")), nil, {provider: provider})
  println(transcript_render_full(result.transcript))
}
"#,
    );
    let fixtures = temp.path().join("recorded.jsonl");

    let recorded = run_harn(
        &temp,
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "record me",
            "--llm-mock-record",
            &fixtures.to_string_lossy(),
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
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "record me",
            "--llm-mock",
            &fixtures.to_string_lossy(),
        ],
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

#[test]
fn playground_llm_mock_sub_agent_tool_calls_mutate_host_workspace() {
    let temp = TempDir::new().unwrap();
    let host = write_file(
        temp.path(),
        "host.harn",
        r#"
pub fn workspace_root() {
  return source_dir()
}

pub fn read_workspace(path) {
  return read_file(path_join(workspace_root(), path))
}

pub fn write_workspace(path, content) {
  let resolved = path_join(workspace_root(), path)
  write_file(resolved, content)
  return resolved
}
"#,
    );
    let script = write_file(
        temp.path(),
        "pipeline.harn",
        r#"
fn tools() {
  var tools = tool_registry()
  tools = tool_define(
    tools,
    "write",
    "Write one file.",
    {
      parameters: {
        path: {type: "string"},
        content: {type: "string"},
      },
      returns: {type: "string"},
      handler: { args -> write_workspace(args.path, args.content) },
    },
  )
  return tools
}

pipeline default() {
  let result = sub_agent_run(
    "Write note.txt with the text hello from fixture.",
    {
      provider: env_or("TEST_PROVIDER", "mock"),
      tools: tools(),
      allowed_tools: ["write"],
      tool_format: "native",
      max_iterations: 2,
    },
  )
  println(result.summary)
  println(json_stringify(result))
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"tool_calls":[{"name":"write","args":{"path":"note.txt","content":"hello from fixture"}}]}
{"text":"write complete"}
"#,
    );

    let output = run_harn(
        &temp,
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "demo",
            "--llm-mock",
            &fixtures,
        ],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );
    let note_contents = fs::read_to_string(temp.path().join("note.txt"));
    assert!(
        note_contents.is_ok(),
        "stdout={}\nstderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(note_contents.unwrap(), "hello from fixture");
    assert!(stdout(&output).contains("write complete"));
}

#[test]
fn playground_llm_mock_sub_agent_handles_multiple_tool_calls_in_one_turn() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("seed.txt"), "seed contents").unwrap();
    let host = write_file(
        temp.path(),
        "host.harn",
        r#"
pub fn workspace_root() {
  return source_dir()
}

pub fn read_workspace(path) {
  return read_file(path_join(workspace_root(), path))
}

pub fn write_workspace(path, content) {
  let resolved = path_join(workspace_root(), path)
  write_file(resolved, content)
  return resolved
}
"#,
    );
    let script = write_file(
        temp.path(),
        "pipeline.harn",
        r#"
fn tools() {
  var tools = tool_registry()
  tools = tool_define(
    tools,
    "read",
    "Read one file.",
    {
      parameters: {path: {type: "string"}},
      returns: {type: "string"},
      handler: { args -> read_workspace(args.path) },
    },
  )
  tools = tool_define(
    tools,
    "write",
    "Write one file.",
    {
      parameters: {
        path: {type: "string"},
        content: {type: "string"},
      },
      returns: {type: "string"},
      handler: { args -> write_workspace(args.path, args.content) },
    },
  )
  return tools
}

pipeline default() {
  let result = sub_agent_run(
    "Read seed.txt and then write note.txt with hello from fixture.",
    {
      provider: env_or("TEST_PROVIDER", "mock"),
      tools: tools(),
      allowed_tools: ["read", "write"],
      tool_format: "native",
      max_iterations: 2,
    },
  )
  println(result.summary)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"tool_calls":[{"name":"read","args":{"path":"seed.txt"}},{"name":"write","args":{"path":"note.txt","content":"hello from fixture"}}]}
{"text":"multi tool complete"}
"#,
    );

    let output = run_harn(
        &temp,
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "demo",
            "--llm-mock",
            &fixtures,
        ],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).unwrap(),
        "hello from fixture"
    );
    assert!(stdout(&output).contains("multi tool complete"));
}

#[test]
fn playground_llm_mock_consume_match_advances_between_identical_patterns() {
    let temp = TempDir::new().unwrap();
    let host = write_file(
        temp.path(),
        "host.harn",
        r#"
pub fn workspace_root() {
  return source_dir()
}

pub fn write_workspace(path, content) {
  let resolved = path_join(workspace_root(), path)
  write_file(resolved, content)
  return resolved
}
"#,
    );
    let script = write_file(
        temp.path(),
        "pipeline.harn",
        r#"
fn tools() {
  var tools = tool_registry()
  tools = tool_define(
    tools,
    "write",
    "Write one file.",
    {
      parameters: {
        path: {type: "string"},
        content: {type: "string"},
      },
      returns: {type: "string"},
      handler: { args -> write_workspace(args.path, args.content) },
    },
  )
  return tools
}

pipeline default() {
  let result = sub_agent_run(
    "[demo][token=write-note]",
    {
      provider: env_or("TEST_PROVIDER", "mock"),
      tools: tools(),
      allowed_tools: ["write"],
      tool_format: "native",
      max_iterations: 2,
    },
  )
  println(result.summary)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"match":"*[demo][token=write-note]*","consume_match":true,"tool_calls":[{"name":"write","args":{"path":"note.txt","content":"matched write"}}]}
{"match":"*[demo][token=write-note]*","consume_match":true,"text":"matched summary"}
"#,
    );

    let output = run_harn(
        &temp,
        &[
            "playground",
            "--host",
            &host,
            "--script",
            &script,
            "--task",
            "demo",
            "--llm-mock",
            &fixtures,
        ],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).unwrap(),
        "matched write"
    );
    assert!(stdout(&output).contains("matched summary"));
}

#[test]
fn eval_runs_baseline_and_structural_variant_for_pipeline_file() {
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "eval_structural.harn",
        r#"
import "std/agents"

pipeline default() {
  let flow = workflow({
    name: "structural-eval",
    persistent: false,
    act: {mode: "llm"},
  })
  let result = task_run("alpha\n\nbeta", flow, {provider: env_or("TEST_PROVIDER", "mock")})
  println(result?.status)
}
"#,
    );
    let fixtures = write_file(
        temp.path(),
        "fixtures.jsonl",
        r#"{"text":"baseline","model":"fixture-model"}
{"text":"variant","model":"fixture-model"}
"#,
    );

    let output = run_harn(
        &temp,
        &[
            "eval",
            "--llm-mock",
            &fixtures,
            "--structural-experiment",
            "doubled_prompt",
            &script,
        ],
        &[("TEST_PROVIDER", "anthropic")],
    );

    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(out.contains("Structural experiment: doubled_prompt"));
    assert!(out.contains("Baseline 1 / 1 passed"));
    assert!(out.contains("Variant 1 / 1 passed"));
}
