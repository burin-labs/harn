use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::cli::{ModelsLoraArgs, ModelsLoraCommand, ModelsLoraInspectArgs, ModelsLoraPlanArgs};
use crate::commands::local::runtime::normalize_local_provider_id;

mod behavior;
mod export;
mod manifest;
mod preflight;
mod promote;
mod promotion_templates;
mod render;
mod train;
mod trainer;

use promotion_templates::lora_promotion_probe_command_templates;
pub(super) use render::{render_embedded_lora_report, run_embedded_lora_report};
use trainer::{
    normalize_lora_trainer, parse_trainer_identity, read_trainer_identity_file,
    trainer_environment_check, trainer_identity_args, trainer_identity_check,
    trainer_identity_from_args, TrainerEnvironmentCheck, TrainerEnvironmentObservation,
    TrainerIdentity, TrainerIdentityCheck,
};

const LORA_INSPECT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_JSON";
const LORA_INSPECT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_PRETTY";
const LORA_PLAN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_JSON";
const LORA_PLAN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_PRETTY";
const LORA_CONTRACT_SCHEMA_VERSION: u64 = 3;
const LORA_CONTRACT_HASH_SCHEMA_VERSION: u64 = 3;
const LORA_TRAINING_CONTRACT_SCHEMA_VERSION: u64 = 3;
const LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION: u64 = 1;
const LORA_TOOL_CATALOG_CONTRACT_SCHEMA_VERSION: u64 = 1;
const LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION: u64 = 5;

pub(crate) async fn run(args: ModelsLoraArgs) {
    let exit_code = match args.command {
        ModelsLoraCommand::Export(args) => Box::pin(export::export_dataset(&args)).await,
        ModelsLoraCommand::Inspect(args) => Box::pin(inspect(&args)).await,
        ModelsLoraCommand::Manifest(args) => Box::pin(manifest::manifest(&args)).await,
        ModelsLoraCommand::Plan(args) => Box::pin(plan(&args)).await,
        ModelsLoraCommand::Preflight(args) => Box::pin(preflight::preflight(&args)).await,
        ModelsLoraCommand::Promote(args) => Box::pin(promote::promote(&args)).await,
        ModelsLoraCommand::Train(args) => Box::pin(train::train(&args)).await,
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn inspect(args: &ModelsLoraInspectArgs) -> i32 {
    let report = match inspect_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    render_embedded_lora_report(
        &report,
        LORA_INSPECT_PAYLOAD_ENV,
        LORA_INSPECT_PAYLOAD_PRETTY_ENV,
        "models/lora_inspect",
        args.json,
        "LoRA inspect",
    )
    .await
}

async fn plan(args: &ModelsLoraPlanArgs) -> i32 {
    let report = match plan_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    render_embedded_lora_report(
        &report,
        LORA_PLAN_PAYLOAD_ENV,
        LORA_PLAN_PAYLOAD_PRETTY_ENV,
        "models/lora_plan",
        args.json,
        "LoRA plan",
    )
    .await
}

mod inspect;
mod normalization;
mod plan;
mod recipes;
mod types;

use inspect::*;
use normalization::*;
use plan::*;
use recipes::*;
use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn default_tool_catalog_contract() -> ToolCatalogContract {
        tool_catalog_contract("full_schema", None, None).expect("default catalog contract")
    }

    fn fixed_tool_catalog_contract() -> ToolCatalogContract {
        tool_catalog_contract(
            "fixed_catalog_internalized",
            Some("burin-tools-v1"),
            Some("sha256:fixedcatalog"),
        )
        .expect("fixed catalog contract")
    }

    fn default_plan_args() -> ModelsLoraPlanArgs {
        ModelsLoraPlanArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            corpus: None,
            teacher: None,
            corpus_strategy: "auto".to_string(),
            method: "qlora".to_string(),
            trainer: "trl_sft_trainer".to_string(),
            trainer_version: None,
            trainer_identity: None,
            rank: 24,
            alpha: None,
            dropout: 0.1,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            json: true,
        }
    }

    #[test]
    fn inspects_local_peft_lora_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "task_type": "CAUSAL_LM",
                "r": 16,
                "lora_alpha": 32,
                "target_modules": ["q_proj", "v_proj"],
                "modules_to_save": ["embed_tokens"]
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(report.ok, "{:?}", report.warnings);
        assert_eq!(report.adapter.peft_type.as_deref(), Some("LORA"));
        assert_eq!(report.adapter.rank, Some(16));
        assert_eq!(
            report.adapter.modules_to_save,
            vec!["embed_tokens".to_string()]
        );
        assert_eq!(report.base.tool_format, "json");
        assert!(!report.tool_calling.native_tools);
        assert_eq!(
            report.compatibility.base_model_match,
            BaseModelMatch::Suffix
        );
        assert!(report.compatibility.provider_supports_lora_launch);
        assert!(report.compatibility.provider_supports_lora_max_rank);
        assert_eq!(report.launch.request_model, "burin-tools");
        assert_eq!(report.launch.max_lora_rank, Some(16));
        assert!(report
            .launch
            .harn_local_launch
            .iter()
            .any(|arg| arg == "--lora-adapter"));
        assert!(report
            .launch
            .harn_local_launch
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "16"]));
    }

    #[test]
    fn inspect_omits_launch_argv_when_provider_lacks_lora_flags() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "r": 16
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("openai".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(report.ok, "{:?}", report.warnings);
        assert!(!report.compatibility.provider_supports_lora_launch);
        assert!(!report.compatibility.provider_supports_lora_max_rank);
        assert_eq!(report.launch.request_model, "burin-tools");
        assert_eq!(report.launch.max_lora_rank, None);
        assert!(report.launch.harn_local_launch.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("provider openai")));
    }

    #[test]
    fn inspect_canonicalizes_local_vllm_provider_alias() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "r": 16
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("local-vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert_eq!(report.base.provider, "vllm");
        assert_eq!(report.serving.provider, "vllm");
        assert!(report.compatibility.provider_supports_lora_launch);
        assert!(report.compatibility.provider_supports_lora_max_rank);
        assert!(report
            .launch
            .harn_local_launch
            .windows(2)
            .any(|pair| pair == ["--provider", "vllm"]));
    }

    #[test]
    fn mismatched_base_model_marks_report_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("other");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{"peft_type":"LORA","base_model_name_or_path":"other/model"}"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: None,
            provider: Some("vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(!report.ok);
        assert_eq!(
            report.compatibility.base_model_match,
            BaseModelMatch::Mismatch
        );
    }

    #[test]
    fn lora_plan_template_selection_keeps_native_gemma4_distinct_from_harn_text() {
        let native = template_recipe_for_route("google/gemma-4-E4B-it", "gemma4", "", "native");
        assert_eq!(native.name, "gemma4_native_function_calling");
        assert!(native
            .requirements
            .iter()
            .any(|item| item.contains("messages plus tools JSON schemas")));

        let json = template_recipe_for_route("google/gemma-4-E4B-it", "gemma4", "", "json");
        assert_eq!(json.name, "harn_text_tool_calls_json_fences");
        assert!(json
            .requirements
            .iter()
            .any(|item| item.contains("Harn before training")));
    }

    #[test]
    fn lora_trainer_contract_keeps_loss_masks_and_tool_columns_explicit() {
        let tool_catalog = default_tool_catalog_contract();
        let native = trainer_contract_for_dataset(
            "messages_with_tool_calls",
            "native",
            "trl_sft_trainer",
            &[],
            &tool_catalog,
        );
        assert!(native
            .iter()
            .any(|item| item.contains("assistant_only_loss=true")));
        assert!(native
            .iter()
            .any(|item| item.contains("messages plus a tools column")));
        assert!(native.iter().any(|item| item.contains("generation masks")));

        let text = trainer_contract_for_dataset(
            "harn_text_tool_calls_json_fences",
            "json",
            "trl_sft_trainer",
            &[],
            &tool_catalog,
        );
        assert!(text.iter().any(|item| item.contains("assistant_tool_text")));
        assert!(text
            .iter()
            .any(|item| item.contains("Harn remains the parser")));

        let native_contract =
            lora_training_contract("messages_with_tool_calls", "native", &[], &tool_catalog);
        assert_eq!(
            native_contract.schema_version,
            LORA_TRAINING_CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            native_contract.assistant_mask_policy,
            "require_chat_template_generation_masks"
        );
        assert_eq!(
            native_contract.tool_parser_owner,
            "provider_tokenizer_runtime"
        );
        assert_eq!(
            native_contract.dataset_split_policy,
            "train_tune_holdout_disjoint_no_eval_holdout_training"
        );

        let text_contract = lora_training_contract(
            "harn_text_tool_calls_json_fences",
            "json",
            &[],
            &tool_catalog,
        );
        assert_eq!(
            text_contract.peft_save_policy.schema_version,
            LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION
        );
        assert_eq!(text_contract.tool_parser_owner, "harn_text_tool_parser");
        assert!(text_contract.peft_save_policy.modules_to_save.is_empty());
        assert!(!text_contract.peft_save_policy.requires_weight_tying_check);

        let embedding_contract = lora_training_contract(
            "harn_text_tool_calls_json_fences",
            "json",
            &["embed_tokens".to_string(), "lm_head".to_string()],
            &tool_catalog,
        );
        assert_eq!(
            embedding_contract.peft_save_policy.modules_to_save,
            vec!["embed_tokens".to_string(), "lm_head".to_string()]
        );
        assert!(
            embedding_contract
                .peft_save_policy
                .requires_weight_tying_check
        );

        let unsloth = trainer_contract_for_dataset(
            "harn_text_tool_calls_json_fences",
            "json",
            "unsloth_sft",
            &[],
            &tool_catalog,
        );
        assert!(unsloth.iter().any(|item| item.contains("Unsloth")));
        assert!(unsloth.iter().any(|item| item.contains("torch/CUDA")));
        assert!(unsloth
            .iter()
            .any(|item| item.contains("modules_to_save=[]")));

        assert_eq!(
            normalize_lora_trainer("mlx-lm").expect("mlx alias"),
            "mlx_lm"
        );
        let mlx = trainer_contract_for_dataset(
            "harn_text_tool_calls_json_fences",
            "json",
            "mlx_lm",
            &[],
            &tool_catalog,
        );
        assert!(mlx.iter().any(|item| item.contains("mlx-lm")));
        assert!(mlx.iter().any(|item| item.contains("Apple Silicon")));
        assert!(mlx
            .iter()
            .any(|item| item.contains("selected local runtime")));
    }

    #[test]
    fn lora_tool_catalog_policy_is_part_of_contract_identity() {
        let default_catalog = default_tool_catalog_contract();
        let fixed_catalog = fixed_tool_catalog_contract();
        let target_modules = TargetModuleContract {
            policy: "all_linear".to_string(),
            modules: vec!["all-linear".to_string()],
        };
        let full_id = lora_contract_id(
            "gemma-4-e4b-it",
            "vllm",
            "json",
            "harn_text_tool_calls_json_fences",
            Some("harn_text_tool_calls_json_fences"),
            &target_modules,
            &[],
            &default_catalog,
        )
        .expect("full-schema contract id");
        let fixed_id = lora_contract_id(
            "gemma-4-e4b-it",
            "vllm",
            "json",
            "harn_text_tool_calls_json_fences",
            Some("harn_text_tool_calls_json_fences"),
            &target_modules,
            &[],
            &fixed_catalog,
        )
        .expect("fixed-catalog contract id");

        assert_ne!(full_id, fixed_id);
        assert_eq!(fixed_catalog.policy, "fixed_catalog_internalized");
        assert_eq!(fixed_catalog.inference_catalog, "no_runtime_catalog");
        assert!(fixed_catalog
            .promotion_gates
            .iter()
            .any(|gate| gate.contains("catalog policy")));
        let fixed_contract = lora_training_contract(
            "harn_text_tool_calls_json_fences",
            "json",
            &[],
            &fixed_catalog,
        );
        assert!(fixed_contract
            .required_example_metadata
            .iter()
            .any(|field| field == "tool_catalog_hash"));
    }

    #[test]
    fn lora_target_modules_are_normalized_and_part_of_contract_identity() {
        let catalog = default_tool_catalog_contract();
        let first = target_module_contract(
            &["v_proj,q_proj".to_string(), "q_proj".to_string()],
            "lora",
            "gemma-4-e4b-it",
            "gemma4",
            "gemma4",
        )
        .expect("explicit target modules");
        let second = target_module_contract(
            &["q_proj".to_string(), "k_proj".to_string()],
            "lora",
            "gemma-4-e4b-it",
            "gemma4",
            "gemma4",
        )
        .expect("different target modules");

        assert_eq!(first.policy, "explicit");
        assert_eq!(first.modules, vec!["q_proj", "v_proj"]);
        assert_ne!(first.modules, second.modules);

        let first_id = lora_contract_id(
            "gemma-4-e4b-it",
            "vllm",
            "json",
            "harn_text_tool_calls_json_fences",
            Some("harn_text_tool_calls_json_fences"),
            &first,
            &[],
            &catalog,
        )
        .expect("first contract id");
        let second_id = lora_contract_id(
            "gemma-4-e4b-it",
            "vllm",
            "json",
            "harn_text_tool_calls_json_fences",
            Some("harn_text_tool_calls_json_fences"),
            &second,
            &[],
            &catalog,
        )
        .expect("second contract id");
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn lora_plan_normalizes_hyperparameters_for_serving_contract() {
        let default_args = default_plan_args();
        let report = plan_report(&default_args).expect("report");
        assert_eq!(report.training.rank, 24);
        assert_eq!(report.training.alpha, 48);
        assert_eq!(report.training.dropout, 0.1);
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "24"]));

        let explicit_args = ModelsLoraPlanArgs {
            alpha: Some(32),
            ..default_args
        };
        let explicit = plan_report(&explicit_args).expect("explicit report");
        assert_eq!(explicit.training.alpha, 32);
    }

    #[test]
    fn lora_plan_canonicalizes_local_vllm_provider_alias() {
        let args = ModelsLoraPlanArgs {
            provider: Some("local-vllm".to_string()),
            corpus: Some("lora-corpus".to_string()),
            ..default_plan_args()
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(report.base.provider, "vllm");
        assert_eq!(report.serving.adapter_binding, "runtime_lora_adapter");
        assert_eq!(
            report.serving.lora_module_value_format,
            "json_with_base_model"
        );
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--provider", "vllm"]));
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "24"]));
    }

    #[test]
    fn lora_plan_records_model_aware_selection_contract() {
        let args = ModelsLoraPlanArgs {
            corpus: Some("lora-corpus".to_string()),
            teacher: Some("dashscope/qwen3-coder-next".to_string()),
            corpus_strategy: "refresh".to_string(),
            trainer: "unsloth_trl_sft".to_string(),
            ..default_plan_args()
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(report.training.trainer, "unsloth_sft");
        let selection = &report.corpus_refresh.model_aware_selection;
        assert!(selection
            .difficulty_signals
            .iter()
            .any(|item| item.contains("target base-model outcome bucket")));
        assert!(selection
            .sampling_policy
            .iter()
            .any(|item| item.contains("medium-difficulty")));
        assert!(selection
            .refinement_loop
            .iter()
            .any(|item| item.contains("parser-valid teacher repairs")));
        assert!(selection
            .stop_conditions
            .iter()
            .any(|item| item.contains("no-write")));
    }

    #[test]
    fn lora_plan_emits_post_training_receipt_and_probe_commands() {
        let args = ModelsLoraPlanArgs {
            corpus: Some("lora-corpus".to_string()),
            teacher: Some("dashscope/qwen3-coder-next".to_string()),
            corpus_strategy: "refresh".to_string(),
            trainer: "unsloth_sft".to_string(),
            alpha: Some(48),
            modules_to_save: vec!["embed_tokens".to_string(), "lm_head".to_string()],
            ..default_plan_args()
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(
            report.training.contract.schema_version,
            LORA_TRAINING_CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            report.training.contract.peft_save_policy.modules_to_save,
            vec!["embed_tokens".to_string(), "lm_head".to_string()]
        );
        assert!(
            report
                .training
                .contract
                .peft_save_policy
                .requires_weight_tying_check
        );
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--modules-to-save", "embed_tokens"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--modules-to-save", "lm_head"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--export-manifest", "ADAPTER_DATASET.manifest.json"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--out", "ADAPTER_OUTPUT_DIR/adapter.manifest.json"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--chat-template", "harn_text_tool_calls_json_fences"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--teacher", "dashscope/qwen3-coder-next"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--trainer", "unsloth_sft"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--trainer", "unsloth_sft"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--receipt-out", "ADAPTER_OUTPUT_DIR/train.receipt.json"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--export-manifest", "ADAPTER_DATASET.manifest.json"]));
        assert!(report.launch.manifest_command.windows(2).any(|pair| pair
            == [
                "--target-metadata",
                "serving_base_precision=same_quantization_family_as_training_or_revalidate"
            ]));
        assert_eq!(
            report.launch.tool_probe_command,
            [
                "harn",
                "provider",
                "tool-probe",
                "vllm",
                "--model",
                "ADAPTER_MODEL",
                "--mode",
                "both",
                "--repeat",
                "5",
                "--json",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            report.launch.promote_command,
            [
                "harn",
                "models",
                "lora",
                "promote",
                "--train-receipt",
                "ADAPTER_OUTPUT_DIR/train.receipt.json",
                "--probe-root",
                "PROMOTION_PROBES",
                "--base-probe-root",
                "BASE_PROMOTION_PROBES",
                "--out",
                "ADAPTER_OUTPUT_DIR/promotion.receipt.json",
                "--check",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
        let evidence = &report.evaluation.evidence_contract;
        assert_eq!(
            evidence.schema_version,
            LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION
        );
        assert!(evidence.promotion_id.starts_with("sha256:"));
        assert_eq!(evidence.base_route.model, report.base.id);
        assert_eq!(evidence.adapter_route.model, "ADAPTER_MODEL");
        assert_eq!(evidence.adapter_route.tool_format, "json");
        assert!(evidence
            .required_receipts
            .iter()
            .any(|receipt| receipt == "lora_adapter_manifest"));
        assert!(evidence
            .required_receipts
            .iter()
            .any(|receipt| receipt == "promotion_probe_matrix_receipt"));
        let probe_case_ids = evidence
            .required_probe_cases
            .iter()
            .map(|probe_case| probe_case.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            probe_case_ids,
            [
                "sequential_tool_call",
                "parallel_tool_calls",
                "no_tool_answer",
                "unavailable_tool_repair",
                "multi_turn_tool_result_continuation",
                "serving_concurrency_probe",
            ]
        );
        assert!(evidence.required_probe_cases.iter().any(|probe_case| {
            probe_case.id == "parallel_tool_calls"
                && probe_case
                    .requirement
                    .contains("required_when_route_supports_parallel_tool_calls")
        }));
        assert_eq!(
            evidence.probe_command_templates.len(),
            evidence.required_probe_cases.len() * 2
        );
        let sequential_probe_command = evidence
            .probe_command_templates
            .iter()
            .find(|template| {
                template.case_id == "sequential_tool_call" && template.route_role == "adapter"
            })
            .expect("sequential adapter probe command template");
        assert_eq!(
            sequential_probe_command.command,
            [
                "harn",
                "eval",
                "tool-calls",
                "--dataset",
                "lora-corpus",
                "--planner",
                "provider=vllm,model=ADAPTER_MODEL",
                "--tool-format",
                "json",
                "--filter",
                "sequential_tool_call",
                "--output",
                "PROMOTION_PROBES/sequential_tool_call",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            sequential_probe_command.summary_path,
            "PROMOTION_PROBES/sequential_tool_call/summary.json"
        );
        let base_sequential_probe_command = evidence
            .probe_command_templates
            .iter()
            .find(|template| {
                template.case_id == "sequential_tool_call" && template.route_role == "base"
            })
            .expect("sequential base probe command template");
        assert_eq!(
            base_sequential_probe_command.command,
            [
                "harn",
                "eval",
                "tool-calls",
                "--dataset",
                "lora-corpus",
                "--planner",
                "provider=vllm,model=gemma-4-e4b-it",
                "--tool-format",
                "json",
                "--filter",
                "sequential_tool_call",
                "--output",
                "BASE_PROMOTION_PROBES/sequential_tool_call",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
        let concurrency_probe_command = evidence
            .probe_command_templates
            .iter()
            .find(|template| {
                template.case_id == "serving_concurrency_probe" && template.route_role == "adapter"
            })
            .expect("serving concurrency probe command template");
        assert!(concurrency_probe_command
            .notes
            .iter()
            .any(|note| note.contains("concurrent adapter-loaded requests")));
        assert!(evidence
            .optional_batch_receipts
            .iter()
            .any(|receipt| receipt == "harn.model_batch_results_receipt"));
        assert_eq!(evidence.batch_ready.workload, "eval");
        assert!(evidence
            .batch_ready
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--id-prefix", "lora-promotion"]));
    }

    #[test]
    fn lora_promotion_id_tracks_acceptance_gate_drift() {
        let metrics = vec!["exact tool-name + argument match rate".to_string()];
        let original_gates = vec!["require a positive paired lift".to_string()];
        let tightened_gates = vec![
            "require a positive paired lift".to_string(),
            "require no non-tool smoke regression".to_string(),
        ];
        let original = PromotionEvidenceInput {
            contract_id: "sha256:contract",
            base_model: "base",
            provider: "vllm",
            request_model: "adapter",
            tool_format: "json",
            eval_dataset: "tool-calls",
            minimum_trials: 5,
            required_metrics: &metrics,
            gates: &original_gates,
            trainer_identity: None,
            trainer_environment: None,
        };
        let required_probe_cases = lora_required_probe_cases(original.tool_format);
        let probe_command_templates =
            lora_promotion_probe_command_templates(&original, &required_probe_cases);
        let original_id =
            lora_promotion_id(&original, &required_probe_cases, &probe_command_templates);
        let tightened = PromotionEvidenceInput {
            gates: &tightened_gates,
            ..original
        };
        let tightened_probe_command_templates =
            lora_promotion_probe_command_templates(&tightened, &required_probe_cases);

        assert_ne!(
            original_id,
            lora_promotion_id(
                &tightened,
                &required_probe_cases,
                &tightened_probe_command_templates,
            )
        );
    }

    #[test]
    fn trainer_environment_attestation_is_canonical_and_binds_promotion() {
        let declared =
            super::trainer::make_trainer_identity("lockfile_sha256", "sha256:declared-lock")
                .expect("declared identity");
        let first: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {"lock_digest": "sha256:resolved-lock", "tool": "uv 0.7.0"},
              "runtime": {"implementation": "CPython", "version": "3.12.3"},
              "packages": {"torch": "2.8.0", "transformers": "4.54.0"},
              "optional_extensions": {"flash_attn": "absent", "unsloth": "present"}
            }"#,
        )
        .expect("first observation");
        let reordered: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "optional_extensions": {"unsloth": "present", "flash_attn": "absent"},
              "packages": {"transformers": "4.54.0", "torch": "2.8.0"},
              "runtime": {"version": "3.12.3", "implementation": "CPython"},
              "resolver": {"tool": "uv 0.7.0", "lock_digest": "sha256:resolved-lock"}
            }"#,
        )
        .expect("reordered observation");
        let first_check = trainer_environment_check(Some(declared.clone()), Some(first));
        let reordered_check = trainer_environment_check(Some(declared.clone()), Some(reordered));
        assert!(first_check.promotable);
        assert_eq!(first_check, reordered_check);

        let changed: TrainerEnvironmentObservation = serde_json::from_str(
            r#"{
              "schema_version": 1,
              "resolver": {"lock_digest": "sha256:resolved-lock", "tool": "uv 0.7.0"},
              "runtime": {"implementation": "CPython", "version": "3.12.3"},
              "packages": {"torch": "2.8.1", "transformers": "4.54.0"},
              "optional_extensions": {"flash_attn": "absent", "unsloth": "present"}
            }"#,
        )
        .expect("changed observation");
        let changed_check = trainer_environment_check(Some(declared.clone()), Some(changed));
        assert!(changed_check.promotable);
        assert_ne!(first_check, changed_check);

        let identity = trainer_identity_check(Some(declared), None);
        let metrics = vec!["exact tool-name + argument match rate".to_string()];
        let gates = vec!["require a positive paired lift".to_string()];
        let input = PromotionEvidenceInput {
            contract_id: "sha256:contract",
            base_model: "base",
            provider: "vllm",
            request_model: "adapter",
            tool_format: "json",
            eval_dataset: "tool-calls",
            minimum_trials: 5,
            required_metrics: &metrics,
            gates: &gates,
            trainer_identity: Some(&identity),
            trainer_environment: Some(&first_check),
        };
        let required_probe_cases = lora_required_probe_cases(input.tool_format);
        let first_templates = lora_promotion_probe_command_templates(&input, &required_probe_cases);
        let first_id = lora_promotion_id(&input, &required_probe_cases, &first_templates);
        let changed_input = PromotionEvidenceInput {
            trainer_environment: Some(&changed_check),
            ..input
        };
        let changed_templates =
            lora_promotion_probe_command_templates(&changed_input, &required_probe_cases);
        assert_ne!(
            first_id,
            lora_promotion_id(&changed_input, &required_probe_cases, &changed_templates)
        );
    }

    #[test]
    fn lora_promotion_id_tracks_probe_matrix_drift() {
        let metrics = vec!["exact tool-name + argument match rate".to_string()];
        let gates = vec!["require a positive paired lift".to_string()];
        let input = PromotionEvidenceInput {
            contract_id: "sha256:contract",
            base_model: "base",
            provider: "vllm",
            request_model: "adapter",
            tool_format: "json",
            eval_dataset: "tool-calls",
            minimum_trials: 5,
            required_metrics: &metrics,
            gates: &gates,
            trainer_identity: None,
            trainer_environment: None,
        };
        let required_probe_cases = lora_required_probe_cases(input.tool_format);
        let probe_command_templates =
            lora_promotion_probe_command_templates(&input, &required_probe_cases);
        let mut changed_probe_cases = required_probe_cases.clone();
        changed_probe_cases[0]
            .expected
            .push_str(" with stable ordering");
        let changed_probe_command_templates =
            lora_promotion_probe_command_templates(&input, &changed_probe_cases);

        assert_ne!(
            lora_promotion_id(&input, &required_probe_cases, &probe_command_templates),
            lora_promotion_id(
                &input,
                &changed_probe_cases,
                &changed_probe_command_templates,
            )
        );
    }

    #[test]
    fn lora_serving_recipe_keeps_runtime_binding_explicit() {
        let tool_catalog = default_tool_catalog_contract();
        let has_requirement = |recipe: &ServingRecipe,
                               kind: &str,
                               name: &str,
                               value: Option<&str>,
                               required: bool| {
            recipe.serving_requirements.iter().any(|requirement| {
                requirement.kind == kind
                    && requirement.name == name
                    && requirement.value.as_deref() == value
                    && requirement.required == required
            })
        };

        let supported = serving_recipe(ServingRecipeInput {
            base_model: "gemma-4-e4b-it",
            provider: "vllm",
            request_model: "ADAPTER_MODEL",
            adapter_name: "ADAPTER_NAME",
            tool_format: "json",
            dataset_format: "harn_text_tool_calls_json_fences",
            provider_supports_lora_launch: true,
            lora_module_value_format: "json_with_base_model",
            tool_catalog: &tool_catalog,
        });
        assert_eq!(supported.adapter_binding, "runtime_lora_adapter");
        assert_eq!(supported.lora_module_value_format, "json_with_base_model");
        assert!(supported
            .runtime_notes
            .iter()
            .any(|note| note.contains("per request model name")));
        assert!(supported
            .runtime_notes
            .iter()
            .any(|note| note.contains("Harn owns tool-call parsing")));
        assert!(has_requirement(
            &supported,
            "parser_owner",
            "tool_call_parser",
            Some("harn_text_tool_parser"),
            true,
        ));
        assert!(has_requirement(
            &supported,
            "provider_native_tool_parser",
            "native_tool_parser_mode",
            Some("disabled_unless_proxy_maps_to_harn_text"),
            true,
        ));

        let external = serving_recipe(ServingRecipeInput {
            base_model: "gemma-4-e4b-it",
            provider: "external",
            request_model: "ADAPTER_MODEL",
            adapter_name: "ADAPTER_NAME",
            tool_format: "json",
            dataset_format: "harn_text_tool_calls_json_fences",
            provider_supports_lora_launch: false,
            lora_module_value_format: "name_path",
            tool_catalog: &tool_catalog,
        });
        assert_eq!(
            external.adapter_binding,
            "external_runtime_or_merged_adapter"
        );
        assert!(external
            .runtime_notes
            .iter()
            .any(|note| note.contains("external runtime")));

        let native_functiongemma = serving_recipe(ServingRecipeInput {
            base_model: "google/functiongemma-270m-it",
            provider: "vllm",
            request_model: "ADAPTER_MODEL",
            adapter_name: "ADAPTER_NAME",
            tool_format: "native",
            dataset_format: "messages_with_tool_calls",
            provider_supports_lora_launch: true,
            lora_module_value_format: "json_with_base_model",
            tool_catalog: &tool_catalog,
        });
        assert!(native_functiongemma
            .runtime_notes
            .iter()
            .any(|note| note.contains("--enable-auto-tool-choice")));
        assert!(native_functiongemma
            .runtime_notes
            .iter()
            .any(|note| note.contains("functiongemma parser/chat template")));
        assert!(has_requirement(
            &native_functiongemma,
            "server_flag",
            "--tool-call-parser",
            Some("functiongemma"),
            true,
        ));
        assert!(has_requirement(
            &native_functiongemma,
            "chat_template",
            "chat_template",
            Some("functiongemma_control_tokens"),
            true,
        ));

        let native_gemma4 = serving_recipe(ServingRecipeInput {
            base_model: "google/gemma-4-e4b-it",
            provider: "vllm",
            request_model: "ADAPTER_MODEL",
            adapter_name: "ADAPTER_NAME",
            tool_format: "native",
            dataset_format: "messages_with_tool_calls",
            provider_supports_lora_launch: true,
            lora_module_value_format: "json_with_base_model",
            tool_catalog: &tool_catalog,
        });
        assert!(has_requirement(
            &native_gemma4,
            "server_flag",
            "--tool-call-parser",
            Some("gemma4"),
            true,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "server_flag",
            "--reasoning-parser",
            Some("gemma4"),
            false,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "chat_template",
            "chat_template",
            Some("examples/tool_chat_template_gemma4.jinja"),
            true,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "manifest_metadata",
            "chat_template_hash",
            None,
            true,
        ));
    }
}
