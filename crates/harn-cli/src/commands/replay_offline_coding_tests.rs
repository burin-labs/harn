#[test]
fn replay_snapshot_retains_application_directories_and_omits_runtime_directories() {
    let workspace = tempfile::tempdir().unwrap();
    for name in [
        "src",
        ".harn-tmp-extra",
        ".harn-tmp",
        ".harn-toolchain-cache",
    ] {
        let directory = workspace.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("note.txt"), name).unwrap();
    }
    let snapshot = super::snapshot_tree(workspace.path()).unwrap();
    assert_eq!(
        snapshot.keys().map(String::as_str).collect::<Vec<_>>(),
        [".harn-tmp-extra/note.txt", "src/note.txt"]
    );
    std::fs::write(workspace.path().join("src/note.txt"), "changed").unwrap();
    let changed = super::snapshot_tree(workspace.path()).unwrap();
    assert_ne!(snapshot["src/note.txt"], changed["src/note.txt"]);
}

use std::collections::BTreeMap;
use std::fs;

use super::{
    compare, execute_once, tree_diff, ExpectedEffect, ExpectedTerminal, ExpectedToolCall,
    NetworkIsolationReceipt, ObservedReplay, ObservedToolCall, OfflineCodingExpected,
    OfflineCodingReplayFixture, ProviderIsolationReceipt,
};

fn fixture() -> OfflineCodingReplayFixture {
    OfflineCodingReplayFixture {
        type_name: "offline_coding_replay".to_string(),
        schema_version: super::OFFLINE_CODING_REPLAY_SCHEMA.to_string(),
        workspace_seed: "seed".to_string(),
        program: "turn.harn".to_string(),
        llm_mock: "turn.jsonl".to_string(),
        effect_root: "repo".to_string(),
        argv: Vec::new(),
        expected: OfflineCodingExpected {
            tools: vec![ExpectedToolCall {
                id: "call-read".to_string(),
                name: "read_file".to_string(),
                status: harn_vm::agent_events::ToolCallStatus::Completed,
                args_blake3: "0".repeat(64),
                result_blake3: "1".repeat(64),
                exit_code: None,
            }],
            effects: vec![ExpectedEffect {
                path: "src/lib.rs".to_string(),
                before_blake3: Some("before".to_string()),
                after_blake3: Some("after".to_string()),
            }],
            terminal: ExpectedTerminal {
                outcome: harn_vm::agent_events::AgentTerminalOutcome::new(
                    harn_vm::agent_events::AgentTerminalKind::Natural,
                    "completed",
                ),
                exit_code: 0,
            },
        },
    }
}

fn completed_verifier_result() -> serde_json::Value {
    serde_json::json!({
        "byte_count": 14,
        "cwd": "$WORKSPACE/repo",
        "denial": null,
        "exit_code": 0,
        "line_count": 1,
        "output_sha256": "sha256:edd98eba7f0634541ba7a5062faec83ab42e51572fdb534054df86a64d55bf15",
        "signal": null,
        "status": "completed",
        "stderr": "",
        "stderr_truncated": false,
        "stdout": "1:hello world\n",
        "stdout_truncated": false,
        "timed_out": false
    })
}

#[cfg(unix)]
fn verifier_golden_results() -> [serde_json::Value; 2] {
    [serde_json::json!(0), serde_json::Value::Null].map(|survivor_count| {
        let mut result = completed_verifier_result();
        result["process_cleanup"] = serde_json::json!({
            "observed_child_count": 0,
            "reaped_child_count": 0,
            "survivor_count": survivor_count,
            "omitted_child_count": 0,
            "observed_children": [],
            "reaped_children": [],
            "surviving_children": []
        });
        result
    })
}

#[cfg(unix)]
fn select_verifier_golden<'a>(observed: Option<&str>, allowed: &'a [String; 2]) -> Option<&'a str> {
    allowed
        .iter()
        .map(String::as_str)
        .find(|known| Some(*known) == observed)
}

#[cfg(unix)]
#[test]
fn verifier_calibration_accepts_only_the_two_authored_terminal_results() {
    let allowed = verifier_golden_results().map(|result| super::canonical_blake3(&result));
    assert_ne!(allowed[0], allowed[1]);
    for hash in &allowed {
        assert!(select_verifier_golden(Some(hash), &allowed).is_some());
    }
    assert!(select_verifier_golden(None, &allowed).is_none());
    let mut null_report = completed_verifier_result();
    null_report["process_cleanup"] = serde_json::Value::Null;
    let mut omitted = verifier_golden_results()[0].clone();
    omitted["process_cleanup"]["omitted_child_count"] = 1.into();
    let mut surviving = verifier_golden_results()[0].clone();
    surviving["process_cleanup"]["survivor_count"] = 1.into();
    surviving["process_cleanup"]["surviving_children"] = serde_json::json!([{
        "alive_after_cleanup": true,
    }]);
    for result in [completed_verifier_result(), null_report, omitted, surviving] {
        let hash = super::canonical_blake3(&result);
        assert!(select_verifier_golden(Some(&hash), &allowed).is_none());
    }
}

#[test]
fn comparison_rejects_projection_only_success_when_effects_diverge() {
    let fixture = fixture();
    let receipt = compare(
        1,
        &fixture,
        ObservedReplay {
            tools: Some(vec![ObservedToolCall {
                id: "call-read".to_string(),
                name: "read_file".to_string(),
                status: harn_vm::agent_events::ToolCallStatus::Completed,
                args_blake3: "0".repeat(64),
                result_blake3: Some("1".repeat(64)),
                exit_code: None,
            }]),
            successful_tools: vec!["read_file".to_string()],
            rejected_tools: Vec::new(),
            effects: Some(Vec::new()),
            terminal: Some(harn_vm::agent_events::AgentTerminalOutcome::new(
                harn_vm::agent_events::AgentTerminalKind::Natural,
                "completed",
            )),
            exit_code: 0,
            execution_error: None,
            observation_errors: Vec::new(),
            provider_isolation: ProviderIsolationReceipt {
                pass: true,
                checkpoint_count: 1,
                builtin_count: 0,
                miss_count: 0,
            },
            network_isolation: NetworkIsolationReceipt {
                pass: true,
                sandbox_enabled: true,
                process_network_allowed: false,
            },
        },
    );
    assert!(
        !receipt.pass,
        "a projected successful terminal must not hide a missing edit"
    );
    assert!(!receipt.effects.pass);
}

#[test]
fn tree_diff_names_created_changed_and_deleted_files() {
    let before = BTreeMap::from([
        ("changed".to_string(), "old".to_string()),
        ("deleted".to_string(), "gone".to_string()),
    ]);
    let after = BTreeMap::from([
        ("changed".to_string(), "new".to_string()),
        ("created".to_string(), "here".to_string()),
    ]);
    assert_eq!(
        tree_diff(&before, &after),
        vec![
            ExpectedEffect {
                path: "changed".to_string(),
                before_blake3: Some("old".to_string()),
                after_blake3: Some("new".to_string()),
            },
            ExpectedEffect {
                path: "created".to_string(),
                before_blake3: None,
                after_blake3: Some("here".to_string()),
            },
            ExpectedEffect {
                path: "deleted".to_string(),
                before_blake3: Some("gone".to_string()),
                after_blake3: None,
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exact_replay_executes_recorded_read_edit_verify_turn() {
    let fixture_dir = tempfile::tempdir().expect("fixture tempdir");
    let seed = fixture_dir.path().join("seed");
    fs::create_dir_all(seed.join("repo")).expect("seed repo");
    fs::write(seed.join("repo/note.txt"), "hello\n").expect("seed file");
    fs::write(
        seed.join("turn.harn"),
        r#"
import { agent_loop } from "std/agent/loop"
import { agent_edit_tools, agent_host_tools } from "std/agent/host_tools"

fn main(harness: Harness) {
  const workspace = argv[0]
  const root = path_join(workspace, "repo")
  const options = {
    root: root,
    cwd: root,
    enabled_tools: ["read_file", "edit_file", "run_command"],
    allow_argv_prefixes: [["grep"]],
  }
  let tools = agent_host_tools(harness, nil, options)
  tools = agent_edit_tools(harness.fs, harness.tools, tools, options)
  const result = agent_loop(
    harness,
    "Read note.txt, change hello to hello world, then verify the result.",
    nil,
    {
      tools: tools,
      tool_format: "native",
      loop_until_done: true,
      max_iterations: 6,
      turn_end_condition: nil,
    },
  )
  return result
}
"#,
    )
    .expect("replay program");
    fs::write(
            seed.join("turn.llm-mock.jsonl"),
            concat!(
                "{\"schemaVersion\":1,\"strictScopes\":true}\n",
                "{\"id\":\"turn-read\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"\",\"tool_calls\":[{\"id\":\"call-read\",\"name\":\"read_file\",\"arguments\":{\"path\":\"note.txt\"}}]}\n",
                "{\"id\":\"turn-edit\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"\",\"tool_calls\":[{\"id\":\"call-edit\",\"name\":\"edit_file\",\"arguments\":{\"path\":\"note.txt\",\"old_string\":\"hello\",\"new_string\":\"hello world\"}}]}\n",
                "{\"id\":\"turn-verify\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"\",\"tool_calls\":[{\"id\":\"call-verify\",\"name\":\"run_command\",\"arguments\":{\"argv\":[\"grep\",\"-n\",\"hello world\",\"note.txt\"]}}]}\n",
                "{\"id\":\"turn-finish\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"Verified.\"}\n",
            ),
        )
        .expect("LLM tape");

    let replay_path = fixture_dir.path().join("replay.json");
    fs::write(&replay_path, "{}\n").expect("fixture anchor");
    let before = blake3::hash(b"hello\n").to_hex().to_string();
    let after = blake3::hash(b"hello world\n").to_hex().to_string();
    let mut fixture = fixture();
    fixture.program = "turn.harn".to_string();
    fixture.llm_mock = "turn.llm-mock.jsonl".to_string();
    fixture.effect_root = "repo".to_string();
    fixture.expected.tools = vec![
        ExpectedToolCall {
            id: "call-read".to_string(),
            name: "read_file".to_string(),
            status: harn_vm::agent_events::ToolCallStatus::Completed,
            args_blake3: super::canonical_blake3(&serde_json::json!({"path":"note.txt"})),
            result_blake3: super::canonical_blake3(&serde_json::json!({
                "content": "hello\n",
                "encoding": "utf-8",
                "path": "$WORKSPACE/repo/note.txt",
                "size": 6,
                "truncated": false
            })),
            exit_code: None,
        },
        ExpectedToolCall {
            id: "call-edit".to_string(),
            name: "edit_file".to_string(),
            status: harn_vm::agent_events::ToolCallStatus::Completed,
            args_blake3: super::canonical_blake3(&serde_json::json!({
                "path":"note.txt",
                "old_string":"hello",
                "new_string":"hello world"
            })),
            result_blake3: super::canonical_blake3(&serde_json::json!({
                "bytes_written": 12,
                "path": "$WORKSPACE/repo/note.txt",
                "replacements": 1
            })),
            exit_code: None,
        },
        ExpectedToolCall {
            id: "call-verify".to_string(),
            name: "run_command".to_string(),
            status: harn_vm::agent_events::ToolCallStatus::Completed,
            args_blake3: super::canonical_blake3(&serde_json::json!({
                "argv":["grep","-n","hello world","note.txt"]
            })),
            result_blake3: {
                #[cfg(unix)]
                let result = verifier_golden_results()[0].clone();
                #[cfg(not(unix))]
                let result = completed_verifier_result();
                super::canonical_blake3(&result)
            },
            exit_code: Some(0),
        },
    ];
    fixture.expected.effects = vec![ExpectedEffect {
        path: "note.txt".to_string(),
        before_blake3: Some(before),
        after_blake3: Some(after),
    }];
    fixture.expected.terminal = ExpectedTerminal {
        outcome: harn_vm::agent_events::AgentTerminalOutcome::new(
            harn_vm::agent_events::AgentTerminalKind::Natural,
            "natural",
        ),
        exit_code: 0,
    };

    // A compatibility host without process confinement records a failed
    // verifier, not an invented successful command. The native release
    // profile rejects that host before changing this fixture expectation.
    #[cfg(target_os = "macos")]
    if let Some(refusal) = crate::tests::common::sandbox_host::unavailable_worktree_sandbox() {
        let verifier = &mut fixture.expected.tools[2];
        verifier.status = harn_vm::agent_events::ToolCallStatus::Failed;
        verifier.result_blake3 =
            super::canonical_blake3(&serde_json::Value::String(refusal.into_error().to_string()));
        verifier.exit_code = None;
    }

    // Calibrate only between the two authored terminal receipts. The exact
    // replay engine then verifies all comparisons in two clean workspaces.
    #[cfg(unix)]
    if fixture.expected.tools[2].status == harn_vm::agent_events::ToolCallStatus::Completed {
        let calibration = execute_once(&replay_path, &fixture, 1)
            .await
            .expect("cleanup capability calibration");
        assert!(calibration.effects.pass && calibration.terminal.pass);
        assert!(calibration.isolation.provider.pass && calibration.isolation.network.pass);
        assert_eq!(calibration.pending_count, 0);
        assert_eq!(calibration.missing_count, 0);
        let allowed = verifier_golden_results().map(|result| super::canonical_blake3(&result));
        let observed = calibration
            .tools
            .actual
            .get(2)
            .and_then(|tool| tool.result_blake3.as_deref());
        fixture.expected.tools[2].result_blake3 = select_verifier_golden(observed, &allowed)
            .unwrap_or_else(|| {
                panic!("verifier result is not an authored terminal receipt: {calibration:#?}")
            })
            .to_string();
    }

    let receipt = execute_once(&replay_path, &fixture, 2)
        .await
        .expect("re-execution setup");
    assert!(receipt.pass, "receipt: {receipt:#?}");

    let clean_repeat = execute_once(&replay_path, &fixture, 3)
        .await
        .expect("second clean-room setup");
    assert!(
        clean_repeat.pass,
        "the first replay must not mutate the next run's seed: {clean_repeat:#?}",
    );

    #[cfg(target_os = "macos")]
    if fixture.expected.tools[2].status == harn_vm::agent_events::ToolCallStatus::Failed {
        fixture.expected.tools[2].status = harn_vm::agent_events::ToolCallStatus::Completed;
        let false_success = execute_once(&replay_path, &fixture, 4)
            .await
            .expect("false success control setup");
        assert!(!false_success.pass && !false_success.tools.pass);
        assert!(
            false_success.effects.pass,
            "only the false command claim must fail"
        );
        fixture.expected.tools[2].status = harn_vm::agent_events::ToolCallStatus::Failed;
    }

    fixture.expected.effects[0].after_blake3 = Some("projected-only".to_string());
    let negative = execute_once(&replay_path, &fixture, 5)
        .await
        .expect("negative control setup");
    assert!(
        !negative.pass,
        "a projected record must not pass without the observed workspace effect"
    );
    assert!(!negative.effects.pass);
}
