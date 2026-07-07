use clap::{Args, Subcommand};

use super::util::llm_model_completion_parser;

#[derive(Debug, Args)]
pub(crate) struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsCommand {
    /// Plan discounted/asynchronous provider batch use for offline workloads.
    Batch(ModelsBatchArgs),
    /// Print resolved metadata for a model alias or model id as JSON.
    Info(ModelInfoArgs),
    /// Inspect LoRA adapter metadata and compatibility with a Harn model route.
    Lora(ModelsLoraArgs),
    /// List models grouped by provider.
    List(ModelsListArgs),
    /// Pull an Ollama model or print setup steps for a known local runtime.
    Install(ModelsInstallArgs),
    /// Recommend a starter model for the current machine and credentials.
    Recommend(ModelRecommendArgs),
    /// Round-trip a small prompt through a model and report timing, tokens, and cost.
    Test(ModelsTestArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchArgs {
    #[command(subcommand)]
    pub command: ModelsBatchCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsBatchCommand {
    /// Cancel submitted batch jobs and write a cancellation receipt.
    Cancel(ModelsBatchCancelArgs),
    /// Download provider result files for completed batch jobs.
    Download(ModelsBatchDownloadArgs),
    /// Write a durable provider-neutral manifest for offline batch requests.
    Manifest(ModelsBatchManifestArgs),
    /// Plan discounted/asynchronous provider batch use for model workloads.
    Plan(ModelsBatchPlanArgs),
    /// Prepare provider-native request files from a model batch manifest.
    Prepare(ModelsBatchPrepareArgs),
    /// Poll provider state for submitted batch jobs and write a status receipt.
    Status(ModelsBatchStatusArgs),
    /// Submit prepared provider-native batch jobs and write a submission receipt.
    Submit(ModelsBatchSubmitArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchCancelArgs {
    /// Submission or status receipt containing provider batch ids.
    #[arg(long, value_name = "PATH")]
    pub receipt: std::path::PathBuf,
    /// Write the cancellation receipt JSON to this path.
    #[arg(long, value_name = "PATH")]
    pub out: std::path::PathBuf,
    /// Validate and render provider operations without calling provider APIs.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchDownloadArgs {
    /// Status receipt produced by `harn models batch status`.
    #[arg(long, value_name = "PATH")]
    pub status: std::path::PathBuf,
    /// Directory for downloaded provider result files and the result receipt.
    #[arg(long = "out-dir", value_name = "DIR")]
    pub out_dir: std::path::PathBuf,
    /// Maximum response body bytes per downloaded provider file.
    #[arg(long = "max-bytes", default_value_t = 268_435_456)]
    pub max_bytes: u64,
    /// Validate result handles and render provider operations without network calls.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchManifestArgs {
    /// JSONL request ledger to group into provider batch jobs.
    #[arg(long, value_name = "PATH")]
    pub requests: std::path::PathBuf,
    /// Write the canonical manifest JSON to this path.
    #[arg(long, value_name = "PATH")]
    pub out: std::path::PathBuf,
    /// Default provider for rows that omit `provider`.
    #[arg(long)]
    pub provider: Option<String>,
    /// Default model alias or provider-native id for rows that omit `model`.
    #[arg(long, value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub model: Option<String>,
    /// Offline workload class (`eval`, `judge`, `corpus`, or `generic`).
    #[arg(long, default_value = "eval")]
    pub workload: String,
    /// Default tool-call convention for rows that omit `tool_format`.
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Prefix for generated custom ids when rows omit `custom_id` / `id`.
    #[arg(long = "id-prefix", default_value = "harn-batch")]
    pub id_prefix: String,
    /// Require at least this published batch discount percentage.
    #[arg(long = "min-discount-percent")]
    pub min_discount_percent: Option<u32>,
    /// Require provider-published completion within this many hours.
    #[arg(long = "max-turnaround-hours")]
    pub max_turnaround_hours: Option<u32>,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchPrepareArgs {
    /// Provider-neutral manifest produced by `harn models batch manifest`.
    #[arg(long, value_name = "PATH")]
    pub manifest: std::path::PathBuf,
    /// Directory for provider-native request files and the prepare receipt.
    #[arg(long = "out-dir", value_name = "DIR")]
    pub out_dir: std::path::PathBuf,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchSubmitArgs {
    /// Prepare receipt produced by `harn models batch prepare`.
    #[arg(long, value_name = "PATH")]
    pub receipt: std::path::PathBuf,
    /// Write the submission receipt JSON to this path.
    #[arg(long, value_name = "PATH")]
    pub out: std::path::PathBuf,
    /// Validate and render provider operations without calling provider APIs.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchStatusArgs {
    /// Submission receipt produced by `harn models batch submit`.
    #[arg(long, value_name = "PATH")]
    pub submission: std::path::PathBuf,
    /// Write the status receipt JSON to this path.
    #[arg(long, value_name = "PATH")]
    pub out: std::path::PathBuf,
    /// Validate and summarize cached job state without calling provider APIs.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsBatchPlanArgs {
    /// Restrict to a single provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Restrict to one model alias or provider-native id.
    #[arg(long, value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub model: Option<String>,
    /// Offline workload class (`eval`, `judge`, `corpus`, or `generic`).
    #[arg(long, default_value = "eval")]
    pub workload: String,
    /// Require at least this published batch discount percentage.
    #[arg(long = "min-discount-percent")]
    pub min_discount_percent: Option<u32>,
    /// Require provider-published completion within this many hours.
    #[arg(long = "max-turnaround-hours")]
    pub max_turnaround_hours: Option<u32>,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraArgs {
    #[command(subcommand)]
    pub command: ModelsLoraCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsLoraCommand {
    /// Export a tool-calling corpus into a trainer-ready LoRA dataset.
    Export(ModelsLoraExportArgs),
    /// Inspect a PEFT LoRA adapter directory or repo id.
    Inspect(ModelsLoraInspectArgs),
    /// Write a canonical LoRA training-run manifest for an adapter.
    Manifest(Box<ModelsLoraManifestArgs>),
    /// Plan a portable LoRA/QLoRA tool-calling fine-tune for a Harn model route.
    Plan(ModelsLoraPlanArgs),
    /// Check a tool-calling corpus before spending GPU time on LoRA training.
    Preflight(ModelsLoraPreflightArgs),
    /// Render or launch a named LoRA trainer backend and write a training receipt.
    Train(Box<ModelsLoraTrainArgs>),
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraExportArgs {
    /// Base model alias or provider-native id the dataset targets.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to plan against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format to export for (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Corpus JSONL file, or a directory containing a conventional corpus JSONL.
    #[arg(long, value_name = "PATH")]
    pub corpus: String,
    /// Write exported JSONL rows to this path. Required unless --check is set.
    #[arg(long, value_name = "PATH")]
    pub out: Option<std::path::PathBuf>,
    /// Write a provenance manifest with input/output hashes and export stats.
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<std::path::PathBuf>,
    /// Validate conversion and print a report without writing dataset rows.
    #[arg(long)]
    pub check: bool,
    /// Served LoRA adapter/model name to include in row metadata.
    #[arg(long = "adapter-name")]
    pub adapter_name: Option<String>,
    /// Chat template identifier to include in row metadata.
    #[arg(long = "chat-template")]
    pub chat_template: Option<String>,
    /// Split assigned to exported rows when source metadata does not declare one.
    #[arg(long = "default-split", default_value = "train")]
    pub default_split: String,
    /// License assigned to exported rows when source metadata does not declare one.
    #[arg(long = "default-license", default_value = "unknown")]
    pub default_license: String,
    /// Extra target provenance copied into row metadata, as KEY=VALUE.
    #[arg(long = "target-metadata", value_name = "KEY=VALUE")]
    pub target_metadata: Vec<String>,
    /// Emit structured JSON report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraInspectArgs {
    /// Base model alias or provider-native id the adapter will attach to.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Adapter directory or Hugging Face repo id.
    pub adapter: String,
    /// Request model name to expose for the adapter. Defaults to the adapter directory/repo basename.
    #[arg(long)]
    pub name: Option<String>,
    /// Provider/runtime to check against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// LoRA export manifest to compare against the adapter and requested route.
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<std::path::PathBuf>,
    /// Fail when the adapter config omits or mismatches the manifest contract id.
    #[arg(long = "require-contract-id")]
    pub require_contract_id: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraManifestArgs {
    /// Base model alias or provider-native id the adapter targets.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to record instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format the adapter was trained to emit (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Trainer dataset path used for this run.
    #[arg(long, value_name = "PATH")]
    pub dataset: Option<std::path::PathBuf>,
    /// Source corpus path used to build the trainer dataset.
    #[arg(long, value_name = "PATH")]
    pub corpus: Option<std::path::PathBuf>,
    /// Harn LoRA export manifest that produced the trainer dataset.
    #[arg(long = "export-manifest", value_name = "PATH")]
    pub export_manifest: Option<std::path::PathBuf>,
    /// Write the manifest JSON to this path. Omit to only print the report.
    #[arg(long, value_name = "PATH")]
    pub out: Option<std::path::PathBuf>,
    /// Served LoRA adapter/model name.
    #[arg(long = "adapter-name")]
    pub adapter_name: Option<String>,
    /// Adapter directory, file, or remote repo id to record.
    #[arg(long = "adapter-path", value_name = "PATH_OR_REPO")]
    pub adapter_path: Option<String>,
    /// Request model name clients should use for the adapter.
    #[arg(long = "request-model")]
    pub request_model: Option<String>,
    /// Chat template identifier used for training and serving.
    #[arg(long = "chat-template")]
    pub chat_template: Option<String>,
    /// Trainer/backend name to record in the manifest.
    #[arg(long, default_value = "external_sft_trainer")]
    pub trainer: String,
    /// Trainer/backend version or package revision to record.
    #[arg(long = "trainer-version")]
    pub trainer_version: Option<String>,
    /// Adapter training method (`qlora` or `lora`).
    #[arg(long, default_value = "qlora")]
    pub method: String,
    /// LoRA rank used for training and serving.
    #[arg(long, default_value_t = 16)]
    pub rank: u32,
    /// LoRA alpha. Defaults to 2 * --rank.
    #[arg(long)]
    pub alpha: Option<u32>,
    /// LoRA dropout probability.
    #[arg(long, default_value_t = 0.05)]
    pub dropout: f64,
    /// Stable training run id from the trainer or orchestration system.
    #[arg(long = "training-run-id")]
    pub training_run_id: Option<String>,
    /// Optional teacher model route used for corpus refresh or distillation.
    #[arg(long, value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub teacher: Option<String>,
    /// Extra target provenance copied into the manifest, as KEY=VALUE.
    #[arg(long = "target-metadata", value_name = "KEY=VALUE")]
    pub target_metadata: Vec<String>,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraPlanArgs {
    /// Base model alias or provider-native id to fine-tune.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to plan against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format to train for (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Corpus path or dataset id to include in the generated eval/training recipe.
    #[arg(long, value_name = "PATH_OR_DATASET")]
    pub corpus: Option<String>,
    /// Optional teacher model route for synthetic corpus refresh or distillation.
    #[arg(long, value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub teacher: Option<String>,
    /// Corpus strategy (`auto`, `audit-only`, `refresh`, or `distill`).
    #[arg(long = "corpus-strategy", default_value = "auto")]
    pub corpus_strategy: String,
    /// Adapter training method (`qlora` or `lora`).
    #[arg(long, default_value = "qlora")]
    pub method: String,
    /// Trainer/backend contract (`trl_sft_trainer`, `unsloth_sft`, or `external_sft_trainer`).
    #[arg(long, default_value = "external_sft_trainer")]
    pub trainer: String,
    /// LoRA rank to plan for training and serving.
    #[arg(long, default_value_t = 16)]
    pub rank: u32,
    /// LoRA alpha. Defaults to 2 * --rank.
    #[arg(long)]
    pub alpha: Option<u32>,
    /// LoRA dropout probability.
    #[arg(long, default_value_t = 0.05)]
    pub dropout: f64,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraTrainArgs {
    /// Base model alias or provider-native id the adapter targets.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to record instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format the adapter dataset targets (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Trainer-ready dataset JSONL produced by `harn models lora export`.
    #[arg(long, value_name = "PATH")]
    pub dataset: std::path::PathBuf,
    /// Source corpus path used to build the trainer dataset.
    #[arg(long, value_name = "PATH")]
    pub corpus: Option<std::path::PathBuf>,
    /// Harn LoRA export manifest that produced the trainer dataset.
    #[arg(long = "export-manifest", value_name = "PATH")]
    pub export_manifest: Option<std::path::PathBuf>,
    /// Directory where the trainer should write adapter artifacts.
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: std::path::PathBuf,
    /// Write the training receipt JSON to this path.
    #[arg(long = "receipt-out", value_name = "PATH")]
    pub receipt_out: Option<std::path::PathBuf>,
    /// Served LoRA adapter/model name.
    #[arg(long = "adapter-name")]
    pub adapter_name: Option<String>,
    /// Request model name clients should use for the adapter.
    #[arg(long = "request-model")]
    pub request_model: Option<String>,
    /// Chat template identifier used for training and serving.
    #[arg(long = "chat-template")]
    pub chat_template: Option<String>,
    /// Trainer/backend contract (`trl_sft_trainer`, `unsloth_sft`, or `external_sft_trainer`).
    #[arg(long, default_value = "external_sft_trainer")]
    pub trainer: String,
    /// Trainer/backend version or package revision to record.
    #[arg(long = "trainer-version")]
    pub trainer_version: Option<String>,
    /// Adapter training method (`qlora` or `lora`).
    #[arg(long, default_value = "qlora")]
    pub method: String,
    /// LoRA rank used for training and serving.
    #[arg(long, default_value_t = 16)]
    pub rank: u32,
    /// LoRA alpha. Defaults to 2 * --rank.
    #[arg(long)]
    pub alpha: Option<u32>,
    /// LoRA dropout probability.
    #[arg(long, default_value_t = 0.05)]
    pub dropout: f64,
    /// Maximum sequence length passed to the backend when it supports one.
    #[arg(long = "max-seq-length")]
    pub max_seq_length: Option<u64>,
    /// Optional teacher model route used for corpus refresh or distillation.
    #[arg(long, value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub teacher: Option<String>,
    /// Extra target provenance copied into the receipt and manifest command, as KEY=VALUE.
    #[arg(long = "target-metadata", value_name = "KEY=VALUE")]
    pub target_metadata: Vec<String>,
    /// Execute the backend argv after rendering the receipt plan. Omit for deterministic dry-run.
    #[arg(long)]
    pub execute: bool,
    /// Backend working directory. Defaults to the current process directory.
    #[arg(long = "backend-cwd", value_name = "DIR")]
    pub backend_cwd: Option<std::path::PathBuf>,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
    /// Backend command argv. Pass after `--`, for example: `-- uv run python train.py config.yaml`.
    #[arg(last = true, value_name = "BACKEND_ARGV")]
    pub backend_argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraPreflightArgs {
    /// Base model alias or provider-native id the adapter will target.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to check against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format the exported dataset and adapter route will target (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Corpus JSONL file, or a directory containing a conventional corpus JSONL.
    #[arg(long, value_name = "PATH")]
    pub corpus: String,
    /// Training config file to read max_seq_length/min_fit_ratio from.
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// Override the config's max_seq_length.
    #[arg(long = "max-seq-length")]
    pub max_seq_length: Option<u64>,
    /// Override the config's min_fit_ratio.
    #[arg(long = "min-fit-ratio")]
    pub min_fit_ratio: Option<f64>,
    /// Hard approximate token budget outlier ceiling.
    #[arg(long = "hard-token-limit", default_value_t = 32_768)]
    pub hard_token_limit: u64,
    /// Minimum trainable record count.
    #[arg(long = "min-records", default_value_t = 1)]
    pub min_records: u64,
    /// Expected source tool-call body format (`json`, `text`, or `auto`).
    #[arg(long = "source-tool-format", default_value = "json")]
    pub source_tool_format: String,
    /// Minimum share of tool calls matching --source-tool-format.
    #[arg(long = "min-tool-call-share", default_value_t = 0.95)]
    pub min_tool_call_share: f64,
    /// Require the last assistant message in each trainable record to contain this marker.
    #[arg(long = "done-marker")]
    pub done_marker: Option<String>,
    /// Exit non-zero when readiness checks fail.
    #[arg(long)]
    pub check: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelInfoArgs {
    /// Verify provider-local readiness for the resolved model when supported.
    #[arg(long)]
    pub verify: bool,
    /// Warm/preload the resolved model when supported. Implies --verify.
    #[arg(long)]
    pub warm: bool,
    /// Ollama keep_alive value to use with --warm (for example 30m, forever, or -1).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
    /// Model alias or provider-native model id.
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsListArgs {
    /// Restrict to a single provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit JSON instead of a human table.
    #[arg(long)]
    pub json: bool,
    /// Only show locally-installed (Ollama) models.
    #[arg(long = "installed-only")]
    pub installed_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsInstallArgs {
    /// Model alias or provider-native id to install or set up.
    pub model: String,
    /// Skip the size-confirmation prompt.
    #[arg(long)]
    pub yes: bool,
    /// Optional Ollama keep-alive hint (e.g. `5m`, `1h`).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelRecommendArgs {
    /// Emit the recommendation and hardware snapshot as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsTestArgs {
    /// Model alias or provider-native model id.
    pub model: String,
    /// Prompt text to send to the model.
    #[arg(long, default_value = "Reply with the word pong.")]
    pub prompt: String,
    /// Provider id to use instead of inferring one from the model selector.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit a structured JSON result.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}
