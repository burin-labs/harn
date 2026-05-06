#![recursion_limit = "256"]

//! In-process coverage of `harn run --llm-mock` / `harn playground --llm-mock`
//! / `harn eval --llm-mock` LLM-mock fixture wiring.
//!
//! Tier 1H follow-up (#1131, parent #1106) of the de-flake epic (#1057):
//! these tests previously ran the `harn` binary as a subprocess to exercise
//! the `--llm-mock` / `--llm-mock-record` driver paths. They now call
//! `harn_cli::commands::run::execute_run` and
//! `harn_cli::commands::playground::execute_playground_inputs` directly,
//! plus the workspace-library eval pipeline, asserting on the captured
//! stdout / stderr / exit_code.
//!
//! These tests build their own multi-thread tokio runtime on a dedicated
//! thread, mirroring `harn_cli::run`'s setup, so the LLM-mock thread-local
//! state and `LocalSet` semantics match what `harn` sees in production.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use harn_cli::commands::playground::{execute_playground_inputs, PlaygroundInputs};
use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, env_lock};
use tempfile::TempDir;

fn write_file(dir: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    path
}

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

#[derive(Clone)]
struct EnvOverride {
    key: &'static str,
    value: &'static str,
}

fn run_harn_in_process(
    cwd: PathBuf,
    script: PathBuf,
    llm_mock_mode: CliLlmMockMode,
    env: Vec<EnvOverride>,
) -> RunOutcome {
    run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let original_cwd = std::env::current_dir().ok();
        std::env::set_current_dir(&cwd).expect("set cwd to test workspace");
        let originals: Vec<(&'static str, Option<String>)> = env
            .iter()
            .map(|item| {
                let prev = std::env::var(item.key).ok();
                std::env::set_var(item.key, item.value);
                (item.key, prev)
            })
            .collect();
        let outcome = execute_run(
            &script.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            llm_mock_mode,
            None,
            RunProfileOptions::default(),
        )
        .await;
        for (key, prev) in originals.iter().rev() {
            match prev {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if let Some(prev) = original_cwd {
            let _ = std::env::set_current_dir(prev);
        }
        outcome
    })
}

fn run_playground_in_process(
    cwd: PathBuf,
    host: PathBuf,
    script: PathBuf,
    task: &str,
    llm_mock_mode: CliLlmMockMode,
    env: Vec<EnvOverride>,
) -> Result<String, String> {
    let task = task.to_string();
    run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let original_cwd = std::env::current_dir().ok();
        std::env::set_current_dir(&cwd).expect("set cwd to test workspace");
        let originals: Vec<(&'static str, Option<String>)> = env
            .iter()
            .map(|item| {
                let prev = std::env::var(item.key).ok();
                std::env::set_var(item.key, item.value);
                (item.key, prev)
            })
            .collect();
        let result = execute_playground_inputs(PlaygroundInputs {
            host,
            script,
            task,
            llm: None,
            llm_mock_mode,
        })
        .await;
        for (key, prev) in originals.iter().rev() {
            match prev {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if let Some(prev) = original_cwd {
            let _ = std::env::set_current_dir(prev);
        }
        result
    })
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

    let outcome = run_harn_in_process(
        temp.path().to_path_buf(),
        script,
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    );

    assert_eq!(
        outcome.exit_code, 0,
        "stderr={}\nstdout={}",
        outcome.stderr, outcome.stdout
    );
    assert_eq!(outcome.stdout, "first\nsecond\n");
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

    let outcome = run_harn_in_process(
        temp.path().to_path_buf(),
        script,
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    );

    assert_eq!(
        outcome.exit_code, 0,
        "stderr={}\nstdout={}",
        outcome.stderr, outcome.stdout
    );
    assert_eq!(outcome.stdout, "matched\nmatched\n");
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

    let outcome = run_harn_in_process(
        temp.path().to_path_buf(),
        script,
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    );

    assert_ne!(outcome.exit_code, 0, "stdout={}", outcome.stdout);
    assert!(
        outcome
            .stderr
            .contains("No --llm-mock fixture matched prompt:"),
        "stderr={}",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("this prompt is intentionally unmatched"),
        "stderr={}",
        outcome.stderr
    );
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

    let recorded = run_harn_in_process(
        temp.path().to_path_buf(),
        script.clone(),
        CliLlmMockMode::Record {
            fixture_path: fixtures.clone(),
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "mock",
        }],
    );
    assert_eq!(
        recorded.exit_code, 0,
        "stderr={}\nstdout={}",
        recorded.stderr, recorded.stdout
    );

    let recorded_fixture = fs::read_to_string(&fixtures).unwrap();
    assert_eq!(recorded_fixture.lines().count(), 1);

    let replayed = run_harn_in_process(
        temp.path().to_path_buf(),
        script,
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    );
    assert_eq!(
        replayed.exit_code, 0,
        "stderr={}\nstdout={}",
        replayed.stderr, replayed.stdout
    );

    assert_eq!(recorded.stdout, replayed.stdout);
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

    let stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host,
        script,
        "demo",
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    )
    .expect("playground run succeeds");

    assert_eq!(stdout, "playground replay\n");
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

    let recorded_stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host.clone(),
        script.clone(),
        "record me",
        CliLlmMockMode::Record {
            fixture_path: fixtures.clone(),
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "mock",
        }],
    )
    .expect("record run succeeds");

    let recorded_fixture = fs::read_to_string(&fixtures).unwrap();
    assert_eq!(recorded_fixture.lines().count(), 1);

    let replayed_stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host,
        script,
        "record me",
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    )
    .expect("replay run succeeds");

    assert_eq!(recorded_stdout, replayed_stdout);
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

    let stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host,
        script,
        "demo",
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    )
    .expect("playground run succeeds");

    let note_contents = fs::read_to_string(temp.path().join("note.txt"))
        .expect("note.txt was written by sub-agent");
    assert_eq!(note_contents, "hello from fixture");
    assert!(stdout.contains("write complete"), "stdout={stdout}");
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

    let stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host,
        script,
        "demo",
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    )
    .expect("playground run succeeds");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).unwrap(),
        "hello from fixture"
    );
    assert!(stdout.contains("multi tool complete"), "stdout={stdout}");
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

    let stdout = run_playground_in_process(
        temp.path().to_path_buf(),
        host,
        script,
        "demo",
        CliLlmMockMode::Replay {
            fixture_path: fixtures,
        },
        vec![EnvOverride {
            key: "TEST_PROVIDER",
            value: "anthropic",
        }],
    )
    .expect("playground run succeeds");

    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt")).unwrap(),
        "matched write"
    );
    assert!(stdout.contains("matched summary"), "stdout={stdout}");
}

// `eval_runs_baseline_and_structural_variant_for_pipeline_file` from the old
// subprocess suite exercised `harn eval --structural-experiment doubled_prompt`.
// That driver (`run_structural_experiment_eval` in `crates/harn-cli/src/lib.rs`)
// internally re-spawns the `harn` binary for the baseline and variant runs, so
// it cannot be converted to in-process invocation until the eval driver itself
// is hoisted into a workspace library API. Tracked as future work in #1131 /
// #1106; this comment is kept here so the open task is discoverable from this
// file.
