use std::path::PathBuf;
use std::{cell::RefCell, thread_local};

use serde::{Deserialize, Serialize};

use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

pub(crate) fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{ts}")
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}

pub(crate) fn default_run_dir() -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::runtime_paths::run_root(&base)
}

mod hooks;
pub use hooks::*;

mod compaction;
pub use compaction::*;

mod artifacts;
pub use artifacts::*;

mod policy;
pub use policy::*;

mod workflow;
pub use workflow::*;

mod records;
pub use records::*;

thread_local! {
    static CURRENT_MUTATION_SESSION: RefCell<Option<MutationSessionRecord>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MutationSessionRecord {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub run_id: Option<String>,
    pub worker_id: Option<String>,
    pub execution_kind: Option<String>,
    pub mutation_scope: String,
    /// Declarative per-tool approval policy for this session. When `None`,
    /// the host drives approval out-of-band via `tool/pre_use` (legacy path).
    pub approval_policy: Option<ToolApprovalPolicy>,
}

impl MutationSessionRecord {
    pub fn normalize(mut self) -> Self {
        if self.session_id.is_empty() {
            self.session_id = new_id("session");
        }
        if self.mutation_scope.is_empty() {
            self.mutation_scope = "read_only".to_string();
        }
        self
    }
}

pub fn install_current_mutation_session(session: Option<MutationSessionRecord>) {
    CURRENT_MUTATION_SESSION.with(|slot| {
        *slot.borrow_mut() = session.map(MutationSessionRecord::normalize);
    });
}

pub fn current_mutation_session() -> Option<MutationSessionRecord> {
    CURRENT_MUTATION_SESSION.with(|slot| slot.borrow().clone())
}
pub(crate) fn parse_json_payload<T: for<'de> Deserialize<'de>>(
    json: serde_json::Value,
    label: &str,
) -> Result<T, VmError> {
    let payload = json.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&payload);
    let mut tracker = serde_path_to_error::Track::new();
    let path_deserializer = serde_path_to_error::Deserializer::new(&mut deserializer, &mut tracker);
    T::deserialize(path_deserializer).map_err(|error| {
        let snippet = if payload.len() > 600 {
            format!("{}...", &payload[..600])
        } else {
            payload.clone()
        };
        VmError::Runtime(format!(
            "{label} parse error at {}: {} | payload={}",
            tracker.path(),
            error,
            snippet
        ))
    })
}

pub(crate) fn parse_json_value<T: for<'de> Deserialize<'de>>(
    value: &VmValue,
) -> Result<T, VmError> {
    parse_json_payload(vm_value_to_json(value), "orchestration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[test]
    fn capability_intersection_rejects_privilege_expansion() {
        let ceiling = CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(2),
            ..Default::default()
        };
        let requested = CapabilityPolicy {
            tools: vec!["read".to_string(), "edit".to_string()],
            ..Default::default()
        };
        let error = ceiling.intersect(&requested).unwrap_err();
        assert!(error.contains("host ceiling"));
    }

    #[test]
    fn mutation_session_normalize_fills_defaults() {
        let normalized = MutationSessionRecord::default().normalize();
        assert!(normalized.session_id.starts_with("session_"));
        assert_eq!(normalized.mutation_scope, "read_only");
        assert!(normalized.approval_policy.is_none());
    }

    #[test]
    fn install_current_mutation_session_round_trips() {
        let policy = ToolApprovalPolicy {
            require_approval: vec!["edit*".to_string()],
            ..Default::default()
        };
        install_current_mutation_session(Some(MutationSessionRecord {
            session_id: "session_test".to_string(),
            mutation_scope: "apply_workspace".to_string(),
            approval_policy: Some(policy.clone()),
            ..Default::default()
        }));
        let current = current_mutation_session().expect("session installed");
        assert_eq!(current.session_id, "session_test");
        assert_eq!(current.mutation_scope, "apply_workspace");
        assert_eq!(current.approval_policy.as_ref(), Some(&policy));

        install_current_mutation_session(None);
        assert!(current_mutation_session().is_none());
    }

    #[test]
    fn active_execution_policy_rejects_unknown_bridge_builtin() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(1),
            ..Default::default()
        });
        let error = enforce_current_policy_for_bridge_builtin("custom_host_builtin").unwrap_err();
        pop_execution_policy();
        assert!(matches!(
            error,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ));
    }

    #[test]
    fn active_execution_policy_rejects_mcp_escape_hatch() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(1),
            ..Default::default()
        });
        let error = enforce_current_policy_for_builtin("mcp_connect", &[]).unwrap_err();
        pop_execution_policy();
        assert!(matches!(
            error,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ));
    }

    #[test]
    fn workflow_normalization_upgrades_legacy_act_verify_repair_shape() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "name": "legacy",
            "act": {"mode": "llm"},
            "verify": {"kind": "verify"},
            "repair": {"mode": "agent"},
        }));
        let graph = normalize_workflow_value(&value).unwrap();
        assert_eq!(graph.type_name, "workflow_graph");
        assert!(graph.nodes.contains_key("act"));
        assert!(graph.nodes.contains_key("verify"));
        assert!(graph.nodes.contains_key("repair"));
        assert_eq!(graph.entry, "act");
    }

    #[test]
    fn workflow_normalization_accepts_tool_registry_nodes() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "name": "registry_tools",
            "entry": "implement",
            "nodes": {
                "implement": {
                    "kind": "stage",
                    "mode": "agent",
                    "tools": {
                        "_type": "tool_registry",
                        "tools": [
                            {"name": "read", "description": "Read files"},
                            {"name": "run", "description": "Run commands"}
                        ]
                    }
                }
            },
            "edges": []
        }));
        let graph = normalize_workflow_value(&value).unwrap();
        let node = graph.nodes.get("implement").unwrap();
        assert_eq!(workflow_tool_names(&node.tools), vec!["read", "run"]);
    }

    #[test]
    fn artifact_selection_honors_budget_and_priority() {
        let policy = ContextPolicy {
            max_artifacts: Some(2),
            max_tokens: Some(30),
            prefer_recent: true,
            prefer_fresh: true,
            prioritize_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        };
        let artifacts = vec![
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "a".to_string(),
                kind: "summary".to_string(),
                text: Some("short".to_string()),
                relevance: Some(0.9),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "b".to_string(),
                kind: "summary".to_string(),
                text: Some("this is a much larger artifact body".to_string()),
                relevance: Some(1.0),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "c".to_string(),
                kind: "summary".to_string(),
                text: Some("tiny".to_string()),
                relevance: Some(0.5),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
        ];
        let selected = select_artifacts(artifacts, &policy);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|artifact| artifact.kind == "summary"));
    }

    #[test]
    fn workflow_validation_rejects_condition_without_true_false_edges() {
        let graph = WorkflowGraph {
            entry: "gate".to_string(),
            nodes: BTreeMap::from([(
                "gate".to_string(),
                WorkflowNode {
                    id: Some("gate".to_string()),
                    kind: "condition".to_string(),
                    ..Default::default()
                },
            )]),
            edges: vec![WorkflowEdge {
                from: "gate".to_string(),
                to: "next".to_string(),
                branch: Some("true".to_string()),
                label: None,
            }],
            ..Default::default()
        };
        let report = validate_workflow(&graph, None);
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("true") && error.contains("false")));
    }

    #[test]
    fn replay_fixture_round_trip_passes() {
        let run = RunRecord {
            type_name: "run_record".to_string(),
            id: "run_1".to_string(),
            workflow_id: "wf".to_string(),
            workflow_name: Some("demo".to_string()),
            task: "demo".to_string(),
            status: "completed".to_string(),
            started_at: "1".to_string(),
            finished_at: Some("2".to_string()),
            parent_run_id: None,
            root_run_id: Some("run_1".to_string()),
            stages: vec![RunStageRecord {
                id: "stage_1".to_string(),
                node_id: "act".to_string(),
                kind: "stage".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                branch: Some("success".to_string()),
                started_at: "1".to_string(),
                finished_at: Some("2".to_string()),
                visible_text: Some("done".to_string()),
                private_reasoning: None,
                transcript: None,
                verification: None,
                usage: None,
                artifacts: vec![ArtifactRecord {
                    type_name: "artifact".to_string(),
                    id: "a1".to_string(),
                    kind: "summary".to_string(),
                    text: Some("done".to_string()),
                    created_at: "1".to_string(),
                    ..Default::default()
                }
                .normalize()],
                consumed_artifact_ids: vec![],
                produced_artifact_ids: vec!["a1".to_string()],
                attempts: vec![],
                metadata: BTreeMap::new(),
            }],
            transitions: vec![],
            checkpoints: vec![],
            pending_nodes: vec![],
            completed_nodes: vec!["act".to_string()],
            child_runs: vec![],
            artifacts: vec![],
            policy: CapabilityPolicy::default(),
            execution: None,
            transcript: None,
            usage: None,
            replay_fixture: None,
            trace_spans: vec![],
            tool_recordings: vec![],
            metadata: BTreeMap::new(),
            persisted_path: None,
        };
        let fixture = replay_fixture_from_run(&run);
        let report = evaluate_run_against_fixture(&run, &fixture);
        assert!(report.pass);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn replay_eval_suite_reports_failed_case() {
        let good = RunRecord {
            id: "run_good".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bad = RunRecord {
            id: "run_bad".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let suite = evaluate_run_suite(vec![
            (
                good.clone(),
                replay_fixture_from_run(&good),
                Some("good.json".to_string()),
            ),
            (
                bad.clone(),
                replay_fixture_from_run(&good),
                Some("bad.json".to_string()),
            ),
        ]);
        assert!(!suite.pass);
        assert_eq!(suite.total, 2);
        assert_eq!(suite.failed, 1);
        assert!(suite.cases.iter().any(|case| !case.pass));
    }

    #[test]
    fn run_diff_reports_changed_stage() {
        let left = RunRecord {
            id: "left".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let right = RunRecord {
            id: "right".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let diff = diff_run_records(&left, &right);
        assert!(diff.status_changed);
        assert!(!diff.identical);
        assert_eq!(diff.stage_diffs.len(), 1);
    }

    #[test]
    fn eval_suite_manifest_can_fail_on_baseline_diff() {
        let temp_dir =
            std::env::temp_dir().join(format!("harn-eval-suite-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let baseline_path = temp_dir.join("baseline.json");
        let candidate_path = temp_dir.join("candidate.json");

        let baseline = RunRecord {
            id: "baseline".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let candidate = RunRecord {
            id: "candidate".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        save_run_record(&baseline, Some(baseline_path.to_str().unwrap())).unwrap();
        save_run_record(&candidate, Some(candidate_path.to_str().unwrap())).unwrap();

        let manifest = EvalSuiteManifest {
            base_dir: Some(temp_dir.display().to_string()),
            cases: vec![EvalSuiteCase {
                label: Some("candidate".to_string()),
                run_path: "candidate.json".to_string(),
                fixture_path: None,
                compare_to: Some("baseline.json".to_string()),
            }],
            ..Default::default()
        };
        let suite = evaluate_run_suite_manifest(&manifest).unwrap();
        assert!(!suite.pass);
        assert_eq!(suite.failed, 1);
        assert!(suite.cases[0].comparison.is_some());
        assert!(suite.cases[0]
            .failures
            .iter()
            .any(|failure| failure.contains("baseline")));
    }

    #[test]
    fn render_unified_diff_marks_removed_and_added_lines() {
        let diff = render_unified_diff(Some("src/main.rs"), "old\nsame", "new\nsame");
        assert!(diff.contains("--- a/src/main.rs"));
        assert!(diff.contains("+++ b/src/main.rs"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
        assert!(diff.contains(" same"));
    }

    #[test]
    fn render_unified_diff_identical_inputs() {
        let text = "line1\nline2\nline3";
        let diff = render_unified_diff(None, text, text);
        assert!(diff.contains("--- a/artifact"));
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert!(!body.iter().any(|l| l.starts_with('-')));
        assert!(!body.iter().any(|l| l.starts_with('+')));
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn render_unified_diff_empty_before() {
        let diff = render_unified_diff(None, "", "new1\nnew2");
        assert!(diff.contains("+new1"));
        assert!(diff.contains("+new2"));
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert!(!body.iter().any(|l| l.starts_with('-')));
    }

    #[test]
    fn render_unified_diff_empty_after() {
        let diff = render_unified_diff(None, "old1\nold2", "");
        assert!(diff.contains("-old1"));
        assert!(diff.contains("-old2"));
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert!(!body.iter().any(|l| l.starts_with('+')));
    }

    #[test]
    fn render_unified_diff_both_empty() {
        let diff = render_unified_diff(None, "", "");
        assert!(diff.contains("--- a/artifact"));
        assert!(diff.contains("+++ b/artifact"));
        // No content lines
        let body: String = diff.lines().skip(2).collect();
        assert!(body.is_empty());
    }

    #[test]
    fn render_unified_diff_all_changed() {
        let diff = render_unified_diff(None, "a\nb", "x\ny");
        assert!(diff.contains("-a"));
        assert!(diff.contains("-b"));
        assert!(diff.contains("+x"));
        assert!(diff.contains("+y"));
    }

    #[test]
    fn render_unified_diff_insertion_in_middle() {
        let diff = render_unified_diff(None, "a\nc", "a\nb\nc");
        assert!(diff.contains(" a"));
        assert!(diff.contains("+b"));
        assert!(diff.contains(" c"));
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert!(!body.iter().any(|l| l.starts_with('-')));
    }

    #[test]
    fn render_unified_diff_deletion_from_middle() {
        let diff = render_unified_diff(None, "a\nb\nc", "a\nc");
        assert!(diff.contains(" a"));
        assert!(diff.contains("-b"));
        assert!(diff.contains(" c"));
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert!(!body.iter().any(|l| l.starts_with('+')));
    }

    #[test]
    fn render_unified_diff_default_path() {
        let diff = render_unified_diff(None, "a", "b");
        assert!(diff.contains("--- a/artifact"));
        assert!(diff.contains("+++ b/artifact"));
    }

    #[test]
    fn render_unified_diff_large_similar() {
        // Test performance: 1000 lines with one change in the middle
        let mut before = Vec::new();
        let mut after = Vec::new();
        for i in 0..1000 {
            before.push(format!("line {i}"));
            after.push(format!("line {i}"));
        }
        before[500] = "OLD LINE 500".to_string();
        after[500] = "NEW LINE 500".to_string();
        let before_str = before.join("\n");
        let after_str = after.join("\n");
        let diff = render_unified_diff(None, &before_str, &after_str);
        assert!(diff.contains("-OLD LINE 500"));
        assert!(diff.contains("+NEW LINE 500"));
        // Context lines should be present
        assert!(diff.contains(" line 499"));
        assert!(diff.contains(" line 501"));
    }

    #[test]
    fn myers_diff_empty_sequences() {
        let ops = myers_diff(&[], &[]);
        assert!(ops.is_empty());
    }

    #[test]
    fn myers_diff_insert_only() {
        let ops = myers_diff(&[], &["a", "b"]);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|(op, _)| *op == DiffOp::Insert));
    }

    #[test]
    fn myers_diff_delete_only() {
        let ops = myers_diff(&["a", "b"], &[]);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|(op, _)| *op == DiffOp::Delete));
    }

    #[test]
    fn myers_diff_equal() {
        let ops = myers_diff(&["a", "b", "c"], &["a", "b", "c"]);
        assert_eq!(ops.len(), 3);
        assert!(ops.iter().all(|(op, _)| *op == DiffOp::Equal));
    }

    #[test]
    fn execution_policy_rejects_process_exec_when_read_only() {
        push_execution_policy(CapabilityPolicy {
            side_effect_level: Some("read_only".to_string()),
            capabilities: BTreeMap::from([("process".to_string(), vec!["exec".to_string()])]),
            ..Default::default()
        });
        let result = enforce_current_policy_for_builtin("exec", &[]);
        pop_execution_policy();
        assert!(result.is_err());
    }

    #[test]
    fn execution_policy_rejects_unlisted_tool() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            ..Default::default()
        });
        let result = enforce_current_policy_for_tool("edit");
        pop_execution_policy();
        assert!(result.is_err());
    }

    #[test]
    fn normalize_run_record_preserves_trace_spans() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "_type": "run_record",
            "id": "run_trace",
            "workflow_id": "wf",
            "status": "completed",
            "started_at": "1",
            "trace_spans": [
                {
                    "span_id": 1,
                    "parent_id": null,
                    "kind": "pipeline",
                    "name": "workflow",
                    "start_ms": 0,
                    "duration_ms": 42,
                    "metadata": {"model": "demo"}
                }
            ]
        }));

        let run = normalize_run_record(&value).unwrap();
        assert_eq!(run.trace_spans.len(), 1);
        assert_eq!(run.trace_spans[0].kind, "pipeline");
        assert_eq!(
            run.trace_spans[0].metadata["model"],
            serde_json::json!("demo")
        );
    }

    // ── Tool hook tests ──────────────────────────────────────────────

    #[test]
    fn pre_tool_hook_deny_blocks_execution() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "dangerous_*".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Deny("blocked by policy".to_string())
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("dangerous_delete", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Deny(_)));
    }

    #[test]
    fn pre_tool_hook_allow_passes_through() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "safe_*".to_string(),
            pre: Some(Rc::new(|_name, _args| PreToolAction::Allow)),
            post: None,
        });
        let result = run_pre_tool_hooks("safe_read", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Allow));
    }

    #[test]
    fn pre_tool_hook_modify_rewrites_args() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "*".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Modify(serde_json::json!({"path": "/sanitized"}))
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("read_file", &serde_json::json!({"path": "/etc/passwd"}));
        clear_tool_hooks();
        match result {
            PreToolAction::Modify(args) => assert_eq!(args["path"], "/sanitized"),
            _ => panic!("expected Modify"),
        }
    }

    #[test]
    fn post_tool_hook_modifies_result() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "exec".to_string(),
            pre: None,
            post: Some(Rc::new(|_name, result| {
                if result.contains("SECRET") {
                    PostToolAction::Modify("[REDACTED]".to_string())
                } else {
                    PostToolAction::Pass
                }
            })),
        });
        let result = run_post_tool_hooks("exec", "output with SECRET data");
        let clean = run_post_tool_hooks("exec", "clean output");
        clear_tool_hooks();
        assert_eq!(result, "[REDACTED]");
        assert_eq!(clean, "clean output");
    }

    #[test]
    fn unmatched_hook_pattern_does_not_fire() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "exec".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Deny("should not match".to_string())
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("read_file", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Allow));
    }

    #[test]
    fn glob_match_patterns() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("exec*", "exec_at"));
        assert!(glob_match("*_file", "read_file"));
        assert!(!glob_match("exec*", "read_file"));
        assert!(glob_match("read_file", "read_file"));
        assert!(!glob_match("read_file", "write_file"));
    }

    // ── Auto-compaction tests ────────────────────────────────────────

    #[test]
    fn microcompact_snips_large_output() {
        let large = "x".repeat(50_000);
        let result = microcompact_tool_output(&large, 10_000);
        assert!(result.len() < 15_000);
        assert!(result.contains("snipped"));
    }

    #[test]
    fn microcompact_preserves_small_output() {
        let small = "hello world";
        let result = microcompact_tool_output(small, 10_000);
        assert_eq!(result, small);
    }

    #[test]
    fn microcompact_preserves_strong_keyword_lines_without_file_line() {
        // Regression: diagnostic extraction used to require both a
        // file:line reference AND a keyword. Strong keywords like "FAIL"
        // and "panic" should preserve the line on their own, because they
        // carry signal even when they appear on narrative lines (Go's
        // "--- FAIL: TestName", Rust's "thread '...' panicked at ...",
        // pytest's "FAILED tests/..."). The exact patterns are language-
        // specific and don't belong in the VM — but the generic rule
        // "strong keywords count even without file:line" does.
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("verbose progress line {i}\n"));
        }
        output.push_str("--- FAIL: TestEmpty (0.00s)\n");
        output.push_str("thread 'tests::test_foo' panicked at src/lib.rs:42:5\n");
        output.push_str("FAILED tests/test_parser.py::test_empty\n");
        for i in 0..100 {
            output.push_str(&format!("more output after failures {i}\n"));
        }
        let result = microcompact_tool_output(&output, 2_000);
        assert!(
            result.contains("--- FAIL: TestEmpty"),
            "strong 'FAIL' keyword should preserve the line:\n{result}"
        );
        assert!(
            result.contains("panicked at"),
            "strong 'panic' keyword should preserve the line:\n{result}"
        );
        assert!(
            result.contains("FAILED tests/test_parser.py"),
            "strong 'FAIL' keyword should preserve pytest-style lines too:\n{result}"
        );
    }

    #[test]
    fn auto_compact_messages_reduces_count() {
        let mut messages: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({"role": "user", "content": format!("message {i}")}))
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let compacted = runtime.block_on(auto_compact_messages(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::Truncate,
                keep_last: 6,
                ..Default::default()
            },
            None,
        ));
        let summary = compacted.unwrap();
        assert!(summary.is_some());
        assert!(messages.len() <= 7); // 6 kept + 1 summary
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("auto-compacted"));
    }

    #[test]
    fn auto_compact_noop_when_under_threshold() {
        let mut messages: Vec<serde_json::Value> = (0..4)
            .map(|i| serde_json::json!({"role": "user", "content": format!("msg {i}")}))
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let compacted = runtime.block_on(auto_compact_messages(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::Truncate,
                keep_last: 6,
                ..Default::default()
            },
            None,
        ));
        assert!(compacted.unwrap().is_none());
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn observation_mask_preserves_errors_masks_verbose_output() {
        // Build a verbose output string (>500 chars) that should be masked
        let verbose_lines: Vec<String> = (0..60)
            .map(|i| format!("// source line {} of the generated file", i))
            .collect();
        let verbose_content = format!(
            "File created: a.go\npackage main\n{}",
            verbose_lines.join("\n")
        );
        let mut messages = vec![
            serde_json::json!({"role": "assistant", "content": "I'll create the file now."}),
            serde_json::json!({"role": "user", "content": verbose_content}),
            serde_json::json!({"role": "assistant", "content": "Now let me run the tests."}),
            serde_json::json!({"role": "user", "content": "error: cannot find module\nexit code 1\nfailed to compile"}),
            serde_json::json!({"role": "assistant", "content": "I see the issue. Let me fix it."}),
            serde_json::json!({"role": "user", "content": "File patched successfully."}),
            // These last 2 will be kept verbatim (keep_last)
            serde_json::json!({"role": "assistant", "content": "Running tests again."}),
            serde_json::json!({"role": "user", "content": "All tests passed."}),
        ];
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let compacted = runtime.block_on(auto_compact_messages(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::ObservationMask,
                keep_last: 2,
                ..Default::default()
            },
            None,
        ));
        let summary = compacted.unwrap().unwrap();
        // Assistant messages preserved verbatim
        assert!(summary.contains("I'll create the file now."));
        assert!(summary.contains("Now let me run the tests."));
        assert!(summary.contains("I see the issue. Let me fix it."));
        // Short error output preserved verbatim (under 500 chars)
        assert!(summary.contains("error: cannot find module"));
        assert!(summary.contains("exit code 1"));
        // Verbose tool output masked (over 500 chars)
        assert!(summary.contains("masked]"));
        assert!(summary.contains("File created: a.go"));
        // Short tool output in kept portion (boundary adjustment moves split_at to user msg)
        assert!(!summary.contains("File patched successfully."));
        // Kept messages not in summary
        assert!(!summary.contains("Running tests again."));
        assert!(!summary.contains("All tests passed."));
        // 3 kept (split moved backward to user boundary) + 1 summary = 4
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn observation_mask_keeps_short_tool_output() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "OK"}),
            serde_json::json!({"role": "user", "content": "Done."}),
        ];
        let summary = observation_mask_compaction(&messages, 2);
        assert!(summary.contains("[user] OK"));
        assert!(summary.contains("[user] Done."));
        assert!(!summary.contains("masked"));
    }

    #[test]
    fn estimate_message_tokens_basic() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "a".repeat(400)}),
            serde_json::json!({"role": "assistant", "content": "b".repeat(400)}),
        ];
        let tokens = estimate_message_tokens(&messages);
        assert_eq!(tokens, 200); // 800 chars / 4
    }

    // ── Artifact dedup and microcompaction tests ─────────────────────

    #[test]
    fn dedup_artifacts_removes_duplicates() {
        let mut artifacts = vec![
            ArtifactRecord {
                id: "a1".to_string(),
                kind: "test".to_string(),
                text: Some("duplicate content".to_string()),
                ..Default::default()
            },
            ArtifactRecord {
                id: "a2".to_string(),
                kind: "test".to_string(),
                text: Some("duplicate content".to_string()),
                ..Default::default()
            },
            ArtifactRecord {
                id: "a3".to_string(),
                kind: "test".to_string(),
                text: Some("unique content".to_string()),
                ..Default::default()
            },
        ];
        dedup_artifacts(&mut artifacts);
        assert_eq!(artifacts.len(), 2);
    }

    #[test]
    fn microcompact_artifact_snips_oversized() {
        let mut artifact = ArtifactRecord {
            id: "a1".to_string(),
            kind: "test".to_string(),
            text: Some("x".repeat(10_000)),
            estimated_tokens: Some(2_500),
            ..Default::default()
        };
        microcompact_artifact(&mut artifact, 500);
        assert!(artifact.text.as_ref().unwrap().len() < 5_000);
        assert_eq!(artifact.estimated_tokens, Some(500));
    }

    // ── Tool argument constraint tests ───────────────────────────────

    #[test]
    fn arg_constraint_allows_matching_pattern() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
                arg_key: Some("command".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "exec",
            &serde_json::json!({"command": "cargo test"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn arg_constraint_rejects_non_matching_pattern() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
                arg_key: Some("command".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "exec",
            &serde_json::json!({"command": "rm -rf /"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn arg_constraint_ignores_unmatched_tool() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
                arg_key: Some("command".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "read_file",
            &serde_json::json!({"path": "/etc/passwd"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn arg_constraint_prefers_declared_path_param_metadata() {
        let mut tool_metadata = std::collections::BTreeMap::new();
        tool_metadata.insert(
            "edit".to_string(),
            ToolRuntimePolicyMetadata {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
        );
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "edit".to_string(),
                arg_patterns: vec!["tests/*".to_string()],
                arg_key: None,
            }],
            tool_metadata,
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "edit",
            &serde_json::json!({
                "action": "replace_range",
                "path": "tests/unit/test_experiment_service.py",
                "content": "..."
            }),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn arg_constraint_without_arg_key_or_metadata_skips_with_warning() {
        // Bug-1 regression: the prior heuristic fallback picked the first
        // string arg (often `action`) and produced misleading errors like
        // "tool 'edit' argument 'exact_patch' does not match …". The new
        // contract requires the policy author to declare either `arg_key`
        // on the constraint or `path_params` in tool metadata. When
        // neither is present the constraint is SKIPPED with a structured
        // `log_warn` — the VM refuses to guess argument semantics by
        // name.
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "edit".to_string(),
                arg_patterns: vec!["tests/unit/test_experiment_service.py".to_string()],
                arg_key: None,
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "edit",
            &serde_json::json!({
                "action": "exact_patch",
                "path": "tests/unit/test_experiment_service.py",
                "old_string": "assert len(items) == 1",
                "new_string": "assert len(items) == 2",
            }),
        );
        assert!(
            result.is_ok(),
            "unresolved constraint must skip (not reject) so a misconfigured policy doesn't silently block work; got: {result:?}"
        );
    }

    #[test]
    fn arg_constraint_with_explicit_arg_key_allows_matching_path() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "edit".to_string(),
                arg_patterns: vec!["tests/unit/*".to_string()],
                arg_key: Some("path".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "edit",
            &serde_json::json!({
                "action": "exact_patch",
                "path": "tests/unit/test_experiment_service.py",
            }),
        );
        assert!(result.is_ok(), "expected allow (path matches), got: {result:?}");
    }

    #[test]
    fn arg_constraint_error_names_the_path_key_not_the_action_value() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "edit".to_string(),
                arg_patterns: vec!["src/allowed/*".to_string()],
                arg_key: Some("path".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "edit",
            &serde_json::json!({
                "action": "replace_range",
                "path": "src/forbidden/foo.rs",
                "content": "..."
            }),
        );
        let Err(err) = result else {
            panic!("expected rejection, got Ok");
        };
        let msg = format!("{err:?}");
        assert!(
            msg.contains("path 'src/forbidden/foo.rs'"),
            "error should name the `path` argument, got: {msg}"
        );
        assert!(
            !msg.contains("argument 'replace_range'"),
            "error must not blame the `action` value, got: {msg}"
        );
    }

    #[test]
    fn arg_constraint_skips_when_no_path_key_present_in_call() {
        // A call that has no value at the declared arg_key is outside the
        // scope of the allow-list — skip the check instead of silently
        // rejecting the empty string against the patterns.
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "edit".to_string(),
                arg_patterns: vec!["tests/*".to_string()],
                arg_key: Some("path".to_string()),
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "edit",
            &serde_json::json!({
                "action": "noop",
                "content": "...",
            }),
        );
        assert!(
            result.is_ok(),
            "no path arg → constraint should skip, got: {result:?}"
        );
    }

    #[test]
    fn microcompact_handles_multibyte_utf8() {
        // Emoji are 4 bytes each — slicing at arbitrary byte offsets would panic
        let emoji_output = "🔥".repeat(500); // 2000 bytes, 500 chars
        let result = microcompact_tool_output(&emoji_output, 400);
        // Should not panic and should contain the snip marker
        assert!(result.contains("snipped"));

        // Mixed ASCII + multi-byte
        let mixed = format!("{}{}{}", "a".repeat(300), "é".repeat(500), "b".repeat(300));
        let result2 = microcompact_tool_output(&mixed, 400);
        assert!(result2.contains("snipped"));

        // CJK characters (3 bytes each)
        let cjk = "中文".repeat(500);
        let result3 = microcompact_tool_output(&cjk, 400);
        assert!(result3.contains("snipped"));
    }

    #[test]
    fn workflow_node_defaults_exit_when_verified_to_false() {
        let node = WorkflowNode::default();
        assert!(!node.exit_when_verified);
    }

    #[test]
    fn workflow_node_exit_when_verified_round_trips_through_serde() {
        let node = WorkflowNode {
            id: Some("execute".to_string()),
            kind: "stage".to_string(),
            exit_when_verified: true,
            ..Default::default()
        };
        let encoded = serde_json::to_value(&node).expect("serialize");
        assert_eq!(
            encoded.get("exit_when_verified"),
            Some(&serde_json::json!(true))
        );
        let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
        assert!(decoded.exit_when_verified);
    }

    #[test]
    fn workflow_node_exit_when_verified_accepts_missing_field_for_backcompat() {
        let encoded = serde_json::json!({
            "id": "legacy_stage",
            "kind": "stage",
        });
        let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
        assert!(
            !decoded.exit_when_verified,
            "nodes serialized before this field was added must deserialize with the default"
        );
    }
}
