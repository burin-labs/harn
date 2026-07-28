use super::{PromotionEvidenceInput, PromotionProbeCase, PromotionProbeCommandTemplate};

pub(super) fn lora_promotion_probe_command_templates(
    input: &PromotionEvidenceInput<'_>,
    required_probe_cases: &[PromotionProbeCase],
) -> Vec<PromotionProbeCommandTemplate> {
    let adapter_planner = format!("provider={},model={}", input.provider, input.request_model);
    let base_planner = format!("provider={},model={}", input.provider, input.base_model);
    required_probe_cases
        .iter()
        .flat_map(|probe_case| {
            ["adapter", "base"].into_iter().map(|route_role| {
                let root = if route_role == "adapter" {
                    "PROMOTION_PROBES"
                } else {
                    "BASE_PROMOTION_PROBES"
                };
                let planner = if route_role == "adapter" {
                    adapter_planner.clone()
                } else {
                    base_planner.clone()
                };
                let output_dir = format!("{root}/{}", probe_case.id);
                let mut notes = Vec::new();
                let mut command = vec![
                    "harn".to_string(),
                    "eval".to_string(),
                    "tool-calls".to_string(),
                    "--dataset".to_string(),
                    input.eval_dataset.to_string(),
                    "--planner".to_string(),
                    planner,
                    "--tool-format".to_string(),
                    input.tool_format.to_string(),
                    "--filter".to_string(),
                    probe_case.id.clone(),
                    "--output".to_string(),
                    output_dir.clone(),
                ];
                if probe_case.id == "serving_concurrency_probe" {
                    command.extend([
                        "--concurrency".to_string(),
                        input.minimum_trials.max(2).to_string(),
                        "--serving-route-role".to_string(),
                        route_role.to_string(),
                    ]);
                    notes.push(if route_role == "adapter" {
                        "runs concurrent adapter-loaded requests and records route identity, parser, request-id, and usage/cost evidence"
                            .to_string()
                    } else {
                        "runs concurrent base-route requests and records route identity, parser, request-id, and usage/cost evidence"
                            .to_string()
                    });
                    if route_role == "adapter" {
                        notes.push(
                            "replace ADAPTER_PATH with the inspected adapter artifact before running"
                                .to_string(),
                        );
                        command.extend([
                            "--serving-adapter-id".to_string(),
                            input.request_model.to_string(),
                            "--serving-adapter-path".to_string(),
                            input.adapter_artifact_path.to_string(),
                            "--serving-adapter-sha256".to_string(),
                            "auto".to_string(),
                        ]);
                    }
                }
                PromotionProbeCommandTemplate {
                    case_id: probe_case.id.clone(),
                    route_role: route_role.to_string(),
                    executor: "harn_eval_tool_calls_filter".to_string(),
                    command,
                    output_dir: output_dir.clone(),
                    summary_path: format!("{output_dir}/summary.json"),
                    per_case_path: format!("{output_dir}/per_case.jsonl"),
                    receipt: probe_case.receipt.clone(),
                    notes,
                }
            })
        })
        .collect()
}
