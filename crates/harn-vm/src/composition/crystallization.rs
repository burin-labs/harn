use std::collections::BTreeSet;

use serde_json::Value;

use super::events::composition_report_events;
use super::types::CompositionExecutionReport;

pub fn composition_crystallization_trace(
    report: &CompositionExecutionReport,
    options: &Value,
) -> Value {
    let trace_id = options
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("composition_{}", report.run.run_id));
    let mut capabilities = BTreeSet::new();
    for call in &report.child_calls {
        if let Some(annotations) = &call.annotations {
            for (domain, ops) in &annotations.capabilities {
                for op in ops {
                    capabilities.insert(format!("{domain}.{op}"));
                }
            }
        }
    }
    let parent_parameters = serde_json::json!({
        "language": report.run.language,
        "snippet_hash": report.run.snippet_hash,
        "binding_manifest_hash": report.run.binding_manifest_hash,
        "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
    });
    let mut actions = vec![serde_json::json!({
        "id": "composition_parent",
        "kind": "composition_run",
        "name": "execute_composition",
        "inputs": parent_parameters,
        "parameters": parent_parameters,
        "output": report.run.result,
        "observed_output": report.run.result,
        "capabilities": capabilities.into_iter().collect::<Vec<_>>(),
        "side_effects": [],
        "duration_ms": report.run.duration_ms.unwrap_or(0),
        "deterministic": true,
        "fuzzy": false,
        "metadata": {
            "source_kind": "composition_parent_run",
            "composition_run_id": report.run.run_id,
            "composition_schema_version": report.schema_version,
            "child_count": report.child_calls.len(),
            "ok": report.ok,
            "failure_category": report.run.failure_category,
        }
    })];
    actions.extend(report.child_calls.iter().map(|call| {
        let result = report
            .child_results
            .iter()
            .find(|result| result.tool_call_id == call.tool_call_id);
        let capabilities = call
            .annotations
            .as_ref()
            .map(|annotations| {
                annotations
                    .capabilities
                    .iter()
                    .flat_map(|(domain, ops)| ops.iter().map(move |op| format!("{domain}.{op}")))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        serde_json::json!({
            "id": format!("composition_child_{}", call.operation_index),
            "kind": "tool_call",
            "name": call.tool_name,
            "inputs": call.raw_input,
            "parameters": call.raw_input,
            "output": result.and_then(|result| result.raw_output.clone()),
            "observed_output": result.and_then(|result| result.raw_output.clone()),
            "capabilities": capabilities,
            "side_effects": [],
            "duration_ms": result.and_then(|result| result.duration_ms).unwrap_or(0),
            "deterministic": true,
            "fuzzy": false,
            "metadata": {
                "source_kind": "composition_child_call",
                "composition_run_id": report.run.run_id,
                "composition_tool_call_id": call.tool_call_id,
                "requested_side_effect_level": call.requested_side_effect_level,
                "annotations": call.annotations,
                "policy_context": call.policy_context,
                "status": result.map(|result| result.status),
                "error_category": result.and_then(|result| result.error_category),
            }
        })
    }));
    let replay_run = composition_replay_run(report, &trace_id);
    serde_json::json!({
        "version": 1,
        "id": trace_id,
        "source": "composition_run",
        "source_hash": report.run.snippet_hash,
        "workflow_id": options.get("workflow_id").and_then(Value::as_str).unwrap_or("composition_candidate"),
        "flow": {
            "trace_id": report.run.run_id,
            "agent_run_id": options.get("agent_run_id").and_then(Value::as_str),
            "transcript_ref": options.get("transcript_ref").and_then(Value::as_str),
        },
        "actions": actions,
        "replay_run": replay_run,
        "replay_allowlist": [
            {
                "path": "/run_id",
                "reason": "run ids are allocated per execution"
            },
            {
                "path": "/effect_receipts/*/run_id",
                "reason": "composition receipts retain source run lineage"
            },
            {
                "path": "/effect_receipts/*/tool_call_id",
                "reason": "composition child call ids include the source run id"
            },
            {
                "path": "/policy_decisions/*/run_id",
                "reason": "composition policy decisions retain source run lineage"
            },
            {
                "path": "/policy_decisions/*/tool_call_id",
                "reason": "composition policy decision ids include the source run id"
            }
        ],
        "metadata": {
            "source_kind": "composition_run",
            "composition_schema_version": report.schema_version,
            "run_id": report.run.run_id,
            "snippet_hash": report.run.snippet_hash,
            "binding_manifest_hash": report.run.binding_manifest_hash,
            "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
            "ok": report.ok,
            "failure_category": report.run.failure_category,
            "child_count": report.child_calls.len(),
        },
    })
}

fn composition_replay_run(report: &CompositionExecutionReport, trace_id: &str) -> Value {
    let event_log_entries = composition_report_events(trace_id, report)
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect::<Vec<_>>();
    let mut effect_receipts = vec![serde_json::json!({
        "kind": "composition_parent",
        "run_id": report.run.run_id,
        "schema_version": report.schema_version,
        "snippet_hash": report.run.snippet_hash,
        "binding_manifest_hash": report.run.binding_manifest_hash,
        "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
        "ok": report.ok,
        "failure_category": report.run.failure_category,
        "result": report.run.result,
        "stdout": report.run.stdout,
    })];
    let mut policy_decisions = Vec::new();
    for call in &report.child_calls {
        let result = report
            .child_results
            .iter()
            .find(|result| result.tool_call_id == call.tool_call_id);
        effect_receipts.push(serde_json::json!({
            "kind": "composition_child",
            "run_id": report.run.run_id,
            "tool_call_id": call.tool_call_id,
            "tool_name": call.tool_name,
            "operation_index": call.operation_index,
            "requested_side_effect_level": call.requested_side_effect_level,
            "input": call.raw_input,
            "status": result.map(|result| result.status),
            "error_category": result.and_then(|result| result.error_category),
            "output": result.and_then(|result| result.raw_output.clone()),
        }));
        policy_decisions.push(serde_json::json!({
            "kind": "composition_child_policy",
            "run_id": report.run.run_id,
            "tool_call_id": call.tool_call_id,
            "tool_name": call.tool_name,
            "requested_side_effect_level": call.requested_side_effect_level,
            "policy_context": call.policy_context,
        }));
    }
    serde_json::json!({
        "run_id": report.run.run_id,
        "event_log_entries": event_log_entries,
        "effect_receipts": effect_receipts,
        "policy_decisions": policy_decisions,
    })
}
