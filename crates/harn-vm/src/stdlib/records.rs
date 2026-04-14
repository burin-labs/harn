//! Artifact and run-record builtins.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::orchestration::{
    diff_run_records, evaluate_run_against_fixture, evaluate_run_suite,
    evaluate_run_suite_manifest, normalize_artifact, normalize_eval_suite_manifest,
    normalize_run_record, render_artifacts_context, render_unified_diff, replay_fixture_from_run,
    save_run_record, select_artifacts, ArtifactRecord, ContextPolicy, ReplayFixture,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::workflow::load_run_tree;

/// A named metric recorded during evaluation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalMetric {
    pub name: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

thread_local! {
    static EVAL_METRICS: RefCell<Vec<EvalMetric>> = const { RefCell::new(Vec::new()) };
}

/// Reset thread-local eval metrics. Call between test runs to avoid leaking.
pub fn reset_eval_metrics() {
    EVAL_METRICS.with(|m| m.borrow_mut().clear());
}

/// Peek at recorded eval metrics without consuming them.
#[allow(dead_code)]
pub(crate) fn peek_eval_metrics() -> Vec<EvalMetric> {
    EVAL_METRICS.with(|m| m.borrow().clone())
}

fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("records encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

pub(crate) fn parse_artifact_list(value: Option<&VmValue>) -> Result<Vec<ArtifactRecord>, VmError> {
    match value {
        Some(VmValue::List(list)) => list.iter().map(normalize_artifact).collect(),
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(_) => Err(VmError::Runtime(
            "expected artifact list or nil".to_string(),
        )),
    }
}

pub(crate) fn parse_context_policy(value: Option<&VmValue>) -> Result<ContextPolicy, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("context policy parse error: {e}"))),
        None => Ok(ContextPolicy::default()),
    }
}

#[derive(Default)]
struct ArtifactHelperOptions {
    id: Option<String>,
    title: Option<String>,
    text: Option<String>,
    source: Option<String>,
    stage: Option<String>,
    freshness: Option<String>,
    priority: Option<i64>,
    relevance: Option<f64>,
    estimated_tokens: Option<usize>,
    lineage: Vec<String>,
    metadata: BTreeMap<String, serde_json::Value>,
    data: Option<serde_json::Value>,
}

fn parse_artifact_helper_options(
    value: Option<&VmValue>,
) -> Result<ArtifactHelperOptions, VmError> {
    let Some(value) = value else {
        return Ok(ArtifactHelperOptions::default());
    };
    match value {
        VmValue::Nil => Ok(ArtifactHelperOptions::default()),
        VmValue::Dict(_) => {
            let json = crate::llm::vm_value_to_json(value);
            let mut options = ArtifactHelperOptions::default();
            let Some(map) = json.as_object() else {
                return Ok(options);
            };
            options.id = map
                .get("id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.title = map
                .get("title")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.text = map
                .get("text")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.source = map
                .get("source")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.stage = map
                .get("stage")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.freshness = map
                .get("freshness")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.priority = map.get("priority").and_then(|value| value.as_i64());
            options.relevance = map.get("relevance").and_then(|value| value.as_f64());
            options.estimated_tokens = map
                .get("estimated_tokens")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize);
            options.lineage = map
                .get("lineage")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(|item| item.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            options.metadata = map
                .get("metadata")
                .and_then(|value| value.as_object())
                .map(|meta| {
                    meta.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            options.data = map.get("data").cloned();
            Ok(options)
        }
        _ => Err(VmError::Runtime(
            "artifact helper options must be a dict or nil".to_string(),
        )),
    }
}

fn require_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn require_text_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(value_to_text)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn optional_text_arg(value: Option<&VmValue>) -> Option<String> {
    value
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(value_to_text)
        .filter(|value| !value.is_empty())
}

fn value_to_text(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.to_string(),
        _ => {
            let json = crate::llm::vm_value_to_json(value);
            if let Some(text) = json.as_str() {
                text.to_string()
            } else {
                json.to_string()
            }
        }
    }
}

fn merge_json_value(
    base: Option<serde_json::Value>,
    overlay: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, overlay) {
        (Some(serde_json::Value::Object(mut base)), Some(serde_json::Value::Object(overlay))) => {
            for (key, value) in overlay {
                base.insert(key, value);
            }
            Some(serde_json::Value::Object(base))
        }
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (Some(_), Some(overlay)) => Some(overlay),
        (None, None) => None,
    }
}

fn build_helper_artifact(
    kind: &str,
    title: Option<String>,
    text: Option<String>,
    data: Option<serde_json::Value>,
    options: ArtifactHelperOptions,
) -> ArtifactRecord {
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: options.id.unwrap_or_default(),
        kind: kind.to_string(),
        title: options.title.or(title),
        text: options.text.or(text),
        data: merge_json_value(data, options.data),
        source: options.source,
        created_at: String::new(),
        freshness: options.freshness,
        priority: options.priority,
        lineage: options.lineage,
        relevance: options.relevance,
        estimated_tokens: options.estimated_tokens,
        stage: options.stage,
        metadata: options.metadata,
    }
    .normalize()
}

pub(crate) fn register_record_builtins(vm: &mut Vm) {
    vm.register_builtin("artifact", |args, _out| {
        let artifact =
            normalize_artifact(args.first().ok_or_else(|| {
                VmError::Runtime("artifact: missing artifact payload".to_string())
            })?)?;
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_derive", |args, _out| {
        let parent = normalize_artifact(
            args.first()
                .ok_or_else(|| VmError::Runtime("artifact_derive: missing parent".to_string()))?,
        )?;
        let kind = args
            .get(1)
            .map(|v| v.display())
            .unwrap_or_else(|| "artifact".to_string());
        let mut derived = parent.clone();
        derived.id = format!("{}_derived", parent.id);
        derived.kind = kind;
        derived.lineage.push(parent.id);
        if let Some(VmValue::Dict(extra)) = args.get(2) {
            let extra_json = crate::llm::vm_value_to_json(&VmValue::Dict(extra.clone()));
            if let Some(text) = extra_json.get("text").and_then(|v| v.as_str()) {
                derived.text = Some(text.to_string());
            }
        }
        to_vm(&derived.normalize())
    });

    vm.register_builtin("artifact_select", |args, _out| {
        let artifacts = parse_artifact_list(args.first())?;
        let policy = parse_context_policy(args.get(1))?;
        to_vm(&select_artifacts(artifacts, &policy))
    });

    vm.register_builtin("artifact_context", |args, _out| {
        let artifacts = parse_artifact_list(args.first())?;
        let policy = parse_context_policy(args.get(1))?;
        Ok(VmValue::String(Rc::from(render_artifacts_context(
            &select_artifacts(artifacts, &policy),
            &policy,
        ))))
    });

    vm.register_builtin("artifact_workspace_file", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_workspace_file", "path")?;
        let content = require_text_arg(args, 1, "artifact_workspace_file", "content")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "workspace_file",
            Some(path.clone()),
            Some(content.clone()),
            Some(serde_json::json!({"path": path, "content": content})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_workspace_snapshot", |args, _out| {
        let paths = args.first().ok_or_else(|| {
            VmError::Runtime("artifact_workspace_snapshot: missing paths".to_string())
        })?;
        let summary = optional_text_arg(args.get(1));
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("paths".to_string(), crate::llm::vm_value_to_json(paths));
        let artifact = build_helper_artifact(
            "workspace_snapshot",
            Some("workspace snapshot".to_string()),
            summary,
            Some(serde_json::json!({"paths": crate::llm::vm_value_to_json(paths)})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_editor_selection", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_editor_selection", "path")?;
        let text = require_text_arg(args, 1, "artifact_editor_selection", "text")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "editor_selection",
            Some(format!("selection {path}")),
            Some(text.clone()),
            Some(serde_json::json!({"path": path, "text": text})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_verification_result", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_verification_result", "title")?;
        let text = require_text_arg(args, 1, "artifact_verification_result", "text")?;
        let artifact = build_helper_artifact(
            "verification_result",
            Some(title.clone()),
            Some(text.clone()),
            Some(serde_json::json!({"title": title, "text": text})),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_test_result", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_test_result", "title")?;
        let text = require_text_arg(args, 1, "artifact_test_result", "text")?;
        let artifact = build_helper_artifact(
            "test_result",
            Some(title.clone()),
            Some(text.clone()),
            Some(serde_json::json!({"title": title, "text": text})),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_command_result", |args, _out| {
        let command = require_string_arg(args, 0, "artifact_command_result", "command")?;
        let output = args.get(1).ok_or_else(|| {
            VmError::Runtime("artifact_command_result: missing output".to_string())
        })?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("command".to_string(), serde_json::json!(command.clone()));
        let artifact = build_helper_artifact(
            "command_result",
            Some(command.clone()),
            Some(value_to_text(output)),
            Some(serde_json::json!({
                "command": command,
                "output": crate::llm::vm_value_to_json(output)
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_diff", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_diff", "path")?;
        let before = require_text_arg(args, 1, "artifact_diff", "before")?;
        let after = require_text_arg(args, 2, "artifact_diff", "after")?;
        let mut options = parse_artifact_helper_options(args.get(3))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "diff",
            Some(format!("diff {path}")),
            Some(render_unified_diff(Some(&path), &before, &after)),
            Some(serde_json::json!({"path": path, "before": before, "after": after})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_git_diff", |args, _out| {
        let diff_text = require_text_arg(args, 0, "artifact_git_diff", "diff_text")?;
        let artifact = build_helper_artifact(
            "git_diff",
            Some("git diff".to_string()),
            Some(diff_text.clone()),
            Some(serde_json::json!({"diff": diff_text})),
            parse_artifact_helper_options(args.get(1))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_diff_review", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_diff_review: missing target artifact".to_string())
        })?)?;
        let summary = optional_text_arg(args.get(1));
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "diff_review",
            Some(format!(
                "review {}",
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            summary,
            Some(serde_json::json!({"target_artifact_id": target.id, "target_kind": target.kind})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_review_decision", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_review_decision: missing target artifact".to_string())
        })?)?;
        let decision = require_string_arg(args, 1, "artifact_review_decision", "decision")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        options
            .metadata
            .insert("decision".to_string(), serde_json::json!(decision.clone()));
        let artifact = build_helper_artifact(
            "review_decision",
            Some(format!(
                "{} {}",
                decision,
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(decision.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "decision": decision
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_patch_proposal", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_patch_proposal: missing target artifact".to_string())
        })?)?;
        let patch = require_text_arg(args, 1, "artifact_patch_proposal", "patch")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "patch_proposal",
            Some(format!(
                "patch for {}",
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(patch.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "patch": patch
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_verification_bundle", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_verification_bundle", "title")?;
        let checks = args.get(1).ok_or_else(|| {
            VmError::Runtime("artifact_verification_bundle: missing checks".to_string())
        })?;
        let artifact = build_helper_artifact(
            "verification_bundle",
            Some(title.clone()),
            Some(value_to_text(checks)),
            Some(serde_json::json!({
                "title": title,
                "checks": crate::llm::vm_value_to_json(checks)
            })),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_apply_intent", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_apply_intent: missing target artifact".to_string())
        })?)?;
        let intent = require_string_arg(args, 1, "artifact_apply_intent", "intent")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "apply_intent",
            Some(format!(
                "{} {}",
                intent,
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(intent.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "intent": intent
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("run_record", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record: missing payload".to_string()))?,
        )?;
        to_vm(&run)
    });

    vm.register_builtin("load_run_tree", |args, _out| {
        let path = require_string_arg(args, 0, "load_run_tree", "path")?;
        let tree = load_run_tree(&path)?;
        to_vm(&tree)
    });

    vm.register_builtin("run_record_save", |args, _out| {
        let mut run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_save: missing run".to_string()))?,
        )?;
        let path = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
        let persisted = save_run_record(&run, path.as_deref())?;
        run.persisted_path = Some(persisted.clone());
        to_vm(&serde_json::json!({"path": persisted, "run": run}))
    });

    vm.register_builtin("run_record_load", |args, _out| {
        let path = args
            .first()
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("run_record_load: missing path".to_string()))?;
        to_vm(&crate::orchestration::load_run_record(
            std::path::Path::new(&path),
        )?)
    });

    vm.register_builtin("run_record_fixture", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_fixture: missing run".to_string()))?,
        )?;
        to_vm(&replay_fixture_from_run(&run))
    });

    vm.register_builtin("run_record_eval", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_eval: missing run".to_string()))?,
        )?;
        let fixture: ReplayFixture = match args.get(1) {
            Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
                .map_err(|e| VmError::Runtime(format!("run_record_eval: {e}")))?,
            None => replay_fixture_from_run(&run),
        };
        to_vm(&evaluate_run_against_fixture(&run, &fixture))
    });

    vm.register_builtin("run_record_eval_suite", |args, _out| {
        let items = match args.first() {
            Some(VmValue::List(list)) => list.clone(),
            _ => {
                return Err(VmError::Runtime(
                    "run_record_eval_suite: missing list".to_string(),
                ));
            }
        };
        let mut cases = Vec::new();
        for item in items.iter() {
            let source_path = item
                .as_dict()
                .and_then(|dict| dict.get("path"))
                .map(|value| value.display())
                .filter(|value| !value.is_empty());
            let run = if let Some(dict) = item.as_dict() {
                if let Some(run_value) = dict.get("run") {
                    normalize_run_record(run_value)?
                } else {
                    normalize_run_record(item)?
                }
            } else {
                normalize_run_record(item)?
            };
            let fixture: ReplayFixture = match item.as_dict().and_then(|dict| dict.get("fixture")) {
                Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
                    .map_err(|e| VmError::Runtime(format!("run_record_eval_suite: {e}")))?,
                None => replay_fixture_from_run(&run),
            };
            cases.push((run, fixture, source_path));
        }
        to_vm(&evaluate_run_suite(cases))
    });

    vm.register_builtin("run_record_diff", |args, _out| {
        let left =
            normalize_run_record(args.first().ok_or_else(|| {
                VmError::Runtime("run_record_diff: missing left run".to_string())
            })?)?;
        let right =
            normalize_run_record(args.get(1).ok_or_else(|| {
                VmError::Runtime("run_record_diff: missing right run".to_string())
            })?)?;
        to_vm(&diff_run_records(&left, &right))
    });

    vm.register_builtin("eval_suite_manifest", |args, _out| {
        let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
            VmError::Runtime("eval_suite_manifest: missing manifest payload".to_string())
        })?)?;
        to_vm(&manifest)
    });

    vm.register_builtin("eval_suite_run", |args, _out| {
        let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
            VmError::Runtime("eval_suite_run: missing manifest payload".to_string())
        })?)?;
        to_vm(&evaluate_run_suite_manifest(&manifest)?)
    });

    vm.register_builtin("eval_metric", |args, _out| {
        let name = args
            .first()
            .map(|v| v.display())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| VmError::Runtime("eval_metric: missing name".to_string()))?;
        let value = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("eval_metric: missing value".to_string()))?;
        let value_json = crate::llm::vm_value_to_json(value);
        let metadata = args
            .get(2)
            .filter(|v| !matches!(v, VmValue::Nil))
            .map(crate::llm::vm_value_to_json);
        EVAL_METRICS.with(|m| {
            m.borrow_mut().push(EvalMetric {
                name,
                value: value_json,
                metadata,
            });
        });
        Ok(VmValue::Nil)
    });

    vm.register_builtin("eval_metrics", |_args, _out| {
        let metrics = EVAL_METRICS.with(|m| m.borrow().clone());
        let list: Vec<VmValue> = metrics
            .iter()
            .map(|metric| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "name".to_string(),
                    VmValue::String(Rc::from(metric.name.as_str())),
                );
                dict.insert(
                    "value".to_string(),
                    crate::stdlib::json_to_vm_value(&metric.value),
                );
                if let Some(ref meta) = metric.metadata {
                    dict.insert(
                        "metadata".to_string(),
                        crate::stdlib::json_to_vm_value(meta),
                    );
                }
                VmValue::Dict(Rc::new(dict))
            })
            .collect();
        Ok(VmValue::List(Rc::from(list)))
    });
}
