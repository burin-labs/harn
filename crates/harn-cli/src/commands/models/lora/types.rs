use serde::Serialize;

use super::{TrainerEnvironmentCheck, TrainerIdentityCheck};

#[derive(Debug, Serialize)]
pub(super) struct LoraInspectReport {
    pub(super) ok: bool,
    pub(super) base: BaseModelReport,
    pub(super) adapter: AdapterReport,
    pub(super) contract: Option<InspectContractReport>,
    pub(super) compatibility: CompatibilityReport,
    pub(super) tool_calling: ToolCallingReport,
    pub(super) serving: InspectServingReport,
    pub(super) launch: LaunchHints,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct BaseModelReport {
    pub(super) selector: String,
    pub(super) id: String,
    pub(super) provider: String,
    pub(super) resolved_alias: Option<String>,
    pub(super) tool_format: String,
    pub(super) tier: String,
    pub(super) family: String,
    pub(super) lineage: String,
    pub(super) catalog_name: Option<String>,
    pub(super) context_window: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(super) struct AdapterReport {
    pub(super) input: String,
    pub(super) name: String,
    pub(super) local_path: Option<String>,
    pub(super) exists: bool,
    pub(super) config_found: bool,
    pub(super) config_path: Option<String>,
    pub(super) weights_found: Vec<String>,
    pub(super) peft_type: Option<String>,
    pub(super) task_type: Option<String>,
    pub(super) base_model_name_or_path: Option<String>,
    pub(super) rank: Option<u64>,
    pub(super) lora_alpha: Option<f64>,
    pub(super) target_modules: Vec<String>,
    pub(super) modules_to_save: Vec<String>,
    pub(super) contract_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct InspectContractReport {
    pub(super) manifest_path: String,
    pub(super) contract_id: Option<String>,
    pub(super) adapter_contract_id: Option<String>,
    pub(super) status: ContractCheckStatus,
    pub(super) base_model_match: BaseModelMatch,
    pub(super) provider_matches: bool,
    pub(super) tool_format_matches: bool,
    pub(super) adapter_name_matches: Option<bool>,
    pub(super) target_modules_match: Option<bool>,
    pub(super) modules_to_save_matches: Option<bool>,
    pub(super) require_adapter_contract_id: bool,
    pub(super) manifest: InspectContractManifest,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct InspectContractManifest {
    pub(super) base_model: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) harn_tool_format: Option<String>,
    pub(super) dataset_format: Option<String>,
    pub(super) chat_template: Option<String>,
    pub(super) target_modules: Option<TargetModuleContract>,
    pub(super) modules_to_save: Option<Vec<String>>,
    pub(super) adapter_name: Option<String>,
    pub(super) request_model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ContractCheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub(super) struct CompatibilityReport {
    pub(super) base_model_match: BaseModelMatch,
    pub(super) provider_supports_lora_launch: bool,
    pub(super) provider_supports_lora_max_rank: bool,
    pub(super) provider_lora_module_value_format: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolCallingReport {
    pub(super) native_tools: bool,
    pub(super) preferred_tool_format: Option<String>,
    pub(super) text_tool_wire_format_supported: bool,
    pub(super) structured_output_mode: String,
    pub(super) recommended_endpoint: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct LaunchHints {
    pub(super) request_model: String,
    pub(super) max_lora_rank: Option<u64>,
    pub(super) harn_local_launch: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct InspectServingReport {
    pub(super) request_model: String,
    pub(super) base_model: String,
    pub(super) provider: String,
    pub(super) tool_format: String,
    pub(super) lora_module_value_format: String,
    pub(super) serving_requirements: Vec<ServingRequirement>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoraPlanReport {
    pub(super) ok: bool,
    pub(super) base: BaseModelReport,
    pub(super) request: PlanRequest,
    pub(super) tool_calling: ToolCallingReport,
    pub(super) training: TrainingRecipe,
    pub(super) precision: PrecisionContract,
    pub(super) template: TemplateRecipe,
    pub(super) data: DataRecipe,
    pub(super) corpus_refresh: CorpusRefreshRecipe,
    pub(super) evaluation: EvaluationRecipe,
    pub(super) serving: ServingRecipe,
    pub(super) launch: PlanLaunchHints,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PlanRequest {
    pub(super) method: String,
    pub(super) requested_tool_format: String,
    pub(super) effective_tool_format: String,
    pub(super) tool_format_correction: Option<String>,
    pub(super) corpus: Option<String>,
    pub(super) requested_corpus_strategy: String,
    pub(super) effective_corpus_strategy: String,
    pub(super) teacher: Option<TeacherReport>,
    pub(super) tool_catalog_policy: String,
    pub(super) tool_catalog_id: Option<String>,
    pub(super) tool_catalog_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TrainingRecipe {
    pub(super) adapter_type: String,
    pub(super) trainer: String,
    pub(super) trainer_version: Option<String>,
    pub(super) trainer_identity: TrainerIdentityCheck,
    pub(super) rank: u32,
    pub(super) alpha: u32,
    pub(super) dropout: f64,
    pub(super) quantization: String,
    pub(super) loss_scope: String,
    pub(super) packing: String,
    pub(super) target_modules: TargetModuleContract,
    pub(super) contract: LoraTrainingContract,
    pub(super) trainer_contract: Vec<String>,
    pub(super) notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PrecisionContract {
    pub(super) schema_version: u64,
    pub(super) training_base_precision: String,
    pub(super) training_compute_precision: String,
    pub(super) adapter_weight_precision: String,
    pub(super) serving_base_precision: String,
    pub(super) serving_adapter_precision: String,
    pub(super) compatibility_policy: String,
    pub(super) promotion_gates: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LoraTrainingContract {
    pub(super) schema_version: u64,
    pub(super) loss_scope: String,
    pub(super) assistant_mask_policy: String,
    pub(super) packing_policy: String,
    pub(super) tool_parser_owner: String,
    pub(super) dataset_format: String,
    pub(super) dataset_split_policy: String,
    pub(super) tool_catalog: ToolCatalogContract,
    pub(super) peft_save_policy: PeftSavePolicy,
    pub(super) required_example_metadata: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ToolCatalogContract {
    pub(super) schema_version: u64,
    pub(super) policy: String,
    pub(super) catalog_id: Option<String>,
    pub(super) catalog_hash: Option<String>,
    pub(super) training_catalog: String,
    pub(super) inference_catalog: String,
    pub(super) schema_columns_required: bool,
    pub(super) prompt_catalog_requirement: String,
    pub(super) notes: Vec<String>,
    pub(super) promotion_gates: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PeftSavePolicy {
    pub(super) schema_version: u64,
    pub(super) modules_to_save: Vec<String>,
    pub(super) save_embedding_layers: String,
    pub(super) tied_embedding_policy: String,
    pub(super) requires_weight_tying_check: bool,
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoraContractReport {
    pub(super) schema_version: u64,
    pub(super) id: String,
    pub(super) base_model: String,
    pub(super) provider: String,
    pub(super) harn_tool_format: String,
    pub(super) dataset_format: String,
    pub(super) chat_template: Option<String>,
    pub(super) target_modules: TargetModuleContract,
    pub(super) training_contract: LoraTrainingContract,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(super) struct TargetModuleContract {
    pub(super) policy: String,
    pub(super) modules: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct LoraContractHashInput<'a> {
    pub(super) schema_version: u64,
    pub(super) base_model: &'a str,
    pub(super) provider: &'a str,
    pub(super) harn_tool_format: &'a str,
    pub(super) dataset_format: &'a str,
    pub(super) chat_template: Option<&'a str>,
    pub(super) target_module_policy: &'a str,
    pub(super) target_modules: &'a [String],
    pub(super) modules_to_save: &'a [String],
    pub(super) tool_catalog_policy: &'a str,
    pub(super) tool_catalog_id: Option<&'a str>,
    pub(super) tool_catalog_hash: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(super) struct TemplateRecipe {
    pub(super) name: String,
    pub(super) source: String,
    pub(super) supervised_target: String,
    pub(super) requirements: Vec<String>,
    pub(super) stop_sequences: Vec<String>,
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct DataRecipe {
    pub(super) dataset_format: String,
    pub(super) required_columns: Vec<String>,
    pub(super) validation: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TeacherReport {
    pub(super) selector: String,
    pub(super) id: String,
    pub(super) provider: String,
    pub(super) resolved_alias: Option<String>,
    pub(super) tool_format: String,
    pub(super) family: String,
    pub(super) lineage: String,
}

#[derive(Debug, Serialize)]
pub(super) struct CorpusRefreshRecipe {
    pub(super) strategy: String,
    pub(super) teacher_required: bool,
    pub(super) teacher: Option<TeacherReport>,
    pub(super) generation_notes: Vec<String>,
    pub(super) provenance_manifest_fields: Vec<String>,
    pub(super) hard_negative_slices: Vec<String>,
    pub(super) acceptance_gates: Vec<String>,
    pub(super) model_aware_selection: ModelAwareSelectionRecipe,
}

#[derive(Debug, Serialize)]
pub(super) struct ModelAwareSelectionRecipe {
    pub(super) objective: String,
    pub(super) difficulty_signals: Vec<String>,
    pub(super) sampling_policy: Vec<String>,
    pub(super) refinement_loop: Vec<String>,
    pub(super) stop_conditions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct EvaluationRecipe {
    pub(super) holdout_policy: String,
    pub(super) minimum_trials: u64,
    pub(super) comparison_baseline: String,
    pub(super) required_metrics: Vec<String>,
    pub(super) gates: Vec<String>,
    pub(super) evidence_contract: PromotionEvidenceContract,
    pub(super) eval_command: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PromotionEvidenceContract {
    pub(super) schema_version: u64,
    pub(super) promotion_id: String,
    pub(super) lora_contract_id: String,
    pub(super) base_route: PromotionRoute,
    pub(super) adapter_route: PromotionRoute,
    pub(super) trainer_identity: Option<TrainerIdentityCheck>,
    pub(super) trainer_environment: Option<TrainerEnvironmentCheck>,
    pub(super) eval_dataset: String,
    pub(super) minimum_trials: u64,
    pub(super) required_receipts: Vec<String>,
    pub(super) required_probe_cases: Vec<PromotionProbeCase>,
    pub(super) probe_command_templates: Vec<PromotionProbeCommandTemplate>,
    pub(super) optional_batch_receipts: Vec<String>,
    pub(super) batch_ready: PromotionBatchReady,
    pub(super) acceptance: PromotionAcceptance,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PromotionProbeCase {
    pub(super) id: String,
    pub(super) requirement: String,
    pub(super) expected: String,
    pub(super) receipt: String,
    pub(super) rationale: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PromotionProbeCommandTemplate {
    pub(super) case_id: String,
    pub(super) route_role: String,
    pub(super) executor: String,
    pub(super) command: Vec<String>,
    pub(super) output_dir: String,
    pub(super) summary_path: String,
    pub(super) per_case_path: String,
    pub(super) receipt: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PromotionRoute {
    pub(super) role: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) tool_format: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PromotionBatchReady {
    pub(super) workload: String,
    pub(super) group_by: Vec<String>,
    pub(super) request_row_contract: Vec<String>,
    pub(super) manifest_command: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PromotionAcceptance {
    pub(super) required_metrics: Vec<String>,
    pub(super) gates: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ServingRecipe {
    pub(super) request_model: String,
    pub(super) adapter_name: String,
    pub(super) base_model: String,
    pub(super) provider: String,
    pub(super) adapter_binding: String,
    pub(super) lora_module_value_format: String,
    pub(super) tool_format: String,
    pub(super) dataset_format: String,
    pub(super) tool_catalog: ToolCatalogContract,
    pub(super) serving_requirements: Vec<ServingRequirement>,
    pub(super) runtime_notes: Vec<String>,
    pub(super) promotion_gates: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ServingRequirement {
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) value: Option<String>,
    pub(super) required: bool,
    pub(super) reason: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PlanLaunchHints {
    pub(super) preflight_command: Vec<String>,
    pub(super) export_command: Vec<String>,
    pub(super) train_command: Vec<String>,
    pub(super) manifest_command: Vec<String>,
    pub(super) inspect_command: Vec<String>,
    pub(super) local_launch_command: Vec<String>,
    pub(super) tool_probe_command: Vec<String>,
    pub(super) promote_command: Vec<String>,
    pub(super) request_model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BaseModelMatch {
    Exact,
    Suffix,
    Mismatch,
    Unknown,
}
