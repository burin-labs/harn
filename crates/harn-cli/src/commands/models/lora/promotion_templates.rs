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
                if probe_case.id == "serving_concurrency_probe" {
                    notes.push(
                        "the selected dataset/evaluator must exercise concurrent adapter-loaded requests; a sequential filtered run is not enough evidence"
                            .to_string(),
                    );
                }
                PromotionProbeCommandTemplate {
                    case_id: probe_case.id.clone(),
                    route_role: route_role.to_string(),
                    executor: "harn_eval_tool_calls_filter".to_string(),
                    command: vec![
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
                    ],
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
