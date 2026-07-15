use std::fs;

use super::lora_fixtures::{
    write_lora_adapter_fixture, write_lora_adapter_fixture_with_contract_and_modules,
    write_lora_corpus_fixture, write_lora_generic_placeholder_corpus_fixture,
    write_lora_probe_summary, write_lora_probe_summary_with_trials,
};
use super::support::{parse_json, run, success_data, LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION};

#[test]
fn models_lora_export_check_reports_native_shape() {
    let corpus = write_lora_corpus_fixture();
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus.path().to_str().expect("utf8 path"),
            "--check",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA export for gemma-4-e4b-it via vllm",
        "dataset format: messages_with_tool_calls",
        "contract: sha256:",
        "contract assistant mask: require_chat_template_generation_masks",
        "contract parser owner: provider_tokenizer_runtime",
        "contract split policy: train_tune_holdout_disjoint_no_eval_holdout_training",
        "provenance defaults: split=train license=unknown",
        "required example metadata:",
        "- tool_schema_hash",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "promotion minimum trials: 5",
        "promotion baseline: same base model, provider, tool format, prompt template, and tool schemas without the adapter",
        "promotion metrics:",
        "native tool-call schema acceptance rate",
        "promotion gates:",
        "require zero contract-id drift between export manifest, adapter metadata, and served route",
        "mode: check",
        "stats: records=6 emitted=5 skipped=1 tool_calls=5 tool_results=5",
        "behavior strata: source=",
        "\"no_tool_answer\":1",
        "\"unavailable_tool_repair\":1",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_preflight_human_text_reports_readiness() {
    let corpus = write_lora_corpus_fixture();
    let config = corpus.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 4096\nmin_fit_ratio: 1.0\n").expect("write config");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA preflight for gemma-4-e4b-it via vllm",
        "target tool format: json",
        "expected source tool format: json",
        "export-required source tool format: json",
        "max_seq_length: 4096",
        "minimum records: 1",
        "required done marker: ##DONE##",
        "records: raw=6 trainable=5 skipped=1",
        "fit: 5/5",
        "tool calls: json=5 text=0 unknown=0 malformed_json=0",
        "behavior strata: source=",
        "\"parallel_tool_call\":1",
        "declared tool formats:",
        "languages:",
        "longest examples:",
        "result: PASS",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_preflight_requires_exportable_source_for_target_format() {
    let corpus = write_lora_corpus_fixture();
    let config = corpus.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 4096\nmin_fit_ratio: 1.0\n").expect("write config");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "text",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--source-tool-format",
            "auto",
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["request"]["target_tool_format"], "text");
    assert_eq!(
        details["thresholds"]["required_export_source_tool_format"],
        "text"
    );
    let errors = details["errors"].as_array().expect("errors");
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("text target requires text source tool calls"))),
        "errors={errors:?}"
    );
}

#[test]
fn models_lora_preflight_json_reports_failures() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = tmp.path().join("burin-tool-calling-corpus.jsonl");
    let config = tmp.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 128\nmin_fit_ratio: 1.0\n").expect("write config");
    let record = serde_json::json!({
        "id": "bad-tool",
        "language": "rust",
        "task_type": "test",
        "eval_name": "bad-tool",
        "metadata": {"tool_format": "json"},
        "messages": [
            {"role": "system", "content": "Available tools: read"},
            {"role": "user", "content": "Fix it."},
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"not_live\",\"arguments\":{}}\n</tool_call>"
            }
        ]
    });
    fs::write(
        &corpus,
        serde_json::to_string(&record).expect("serialize record") + "\n",
    )
    .expect("write corpus");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            tmp.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["stats"]["trainable_records"], 1);
    assert_eq!(
        details["stats"]["records_with_unrecognized_tools"],
        serde_json::Value::from(1)
    );
    let errors = details["errors"].as_array().expect("errors");
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("missing required done marker"))),
        "errors={errors:?}"
    );
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("not declared"))),
        "errors={errors:?}"
    );
}

#[test]
fn models_lora_export_json_writes_dataset_and_manifest() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let manifest = corpus.path().join("structured.manifest.json");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--manifest",
            manifest.to_str().expect("utf8 manifest path"),
            "--adapter-name",
            "burin-tools",
            "--chat-template",
            "gemma-4",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(
        report["request"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(report["stats"]["records"].as_u64(), Some(6));
    assert_eq!(report["stats"]["emitted"].as_u64(), Some(5));
    assert_eq!(report["stats"]["skipped"].as_u64(), Some(1));
    assert_eq!(report["stats"]["tool_calls"].as_u64(), Some(5));
    assert_eq!(report["stats"]["tool_results"].as_u64(), Some(5));
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["no_tool_answer"].as_u64(),
        Some(1)
    );
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["multi_turn_continuation"].as_u64(),
        Some(1)
    );
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    assert_eq!(report["target"]["provider"], "vllm");
    let contract_id = report["contract"]["id"].as_str().expect("contract id");
    assert!(
        contract_id.starts_with("sha256:"),
        "contract id={contract_id}"
    );
    assert_eq!(report["target"]["contract_id"], contract_id);
    assert_eq!(report["contract"]["base_model"], "gemma-4-e4b-it");
    assert_eq!(report["contract"]["provider"], "vllm");
    assert_eq!(report["serving"]["provider"], "vllm");
    assert_eq!(report["contract"]["harn_tool_format"], "native");
    assert_eq!(
        report["contract"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(report["contract"]["chat_template"], "gemma-4");
    assert_eq!(
        report["contract"]["training_contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        report["contract"]["training_contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(
        report["contract"]["training_contract"]["dataset_split_policy"],
        "train_tune_holdout_disjoint_no_eval_holdout_training"
    );
    let required_metadata = report["contract"]["training_contract"]["required_example_metadata"]
        .as_array()
        .expect("required example metadata");
    for field in [
        "source_record_id",
        "source_transcript_id",
        "teacher_model",
        "teacher_provider",
        "target_base_model",
        "target_tool_format",
        "tool_schema_hash",
        "prompt_template_hash",
        "split",
        "license",
    ] {
        assert!(
            required_metadata.iter().any(|value| value == field),
            "required metadata missing {field}: {required_metadata:?}"
        );
    }
    assert_eq!(report["serving"]["request_model"], "burin-tools");
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["serving"]["contract_id"], contract_id);
    assert_eq!(report["promotion"]["minimum_trials"], 5);
    assert_eq!(
        report["promotion"]["comparison_baseline"],
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
    );
    let promotion_eval = report["promotion"]["eval_command"]
        .as_array()
        .expect("promotion eval argv");
    assert!(
        promotion_eval
            .windows(2)
            .any(|pair| pair[0] == "--planner" && pair[1] == "burin-tools"),
        "promotion eval argv={promotion_eval:?}"
    );
    assert!(
        promotion_eval
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "promotion eval argv={promotion_eval:?}"
    );
    assert!(out.is_file(), "exported JSONL missing");
    assert!(manifest.is_file(), "manifest missing");

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let rows = row_text
        .lines()
        .map(|line| parse_json(line, "export row"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 5, "rows={rows:?}");
    let row = rows
        .iter()
        .find(|row| row["id"] == "tiny-read")
        .expect("tiny-read row");
    let messages = row["messages"].as_array().expect("messages array");
    assert!(
        messages.iter().any(|message| message["role"] == "assistant"
            && message["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.len() == 1)),
        "messages={messages:?}"
    );
    assert!(
        messages.iter().any(|message| message["role"] == "tool"
            && message["name"] == "read"
            && message["tool_call_id"] == "call_2_1"),
        "messages={messages:?}"
    );
    let tools = row["tools"].as_array().expect("tools array");
    assert!(
        tools.iter().any(|tool| tool["function"]["name"] == "read"),
        "tools={tools:?}"
    );
    assert_eq!(row["metadata"]["source_tool_format"], "json");
    assert_eq!(row["metadata"]["source_record_id"], "tiny-read");
    assert_eq!(row["metadata"]["source_transcript_id"], "tiny-read");
    assert_eq!(row["metadata"]["teacher_model"], "manual");
    assert_eq!(row["metadata"]["teacher_provider"], "");
    assert_eq!(row["metadata"]["target_base_model"], "gemma-4-e4b-it");
    assert_eq!(row["metadata"]["target_tool_format"], "native");
    assert_eq!(row["metadata"]["split"], "train");
    assert_eq!(row["metadata"]["license"], "unknown");
    for field in ["tool_schema_hash", "prompt_template_hash"] {
        let hash = row["metadata"][field].as_str().expect("metadata hash");
        assert!(
            hash.starts_with("sha256:") && hash.len() == "sha256:".len() + 64,
            "{field}={hash}"
        );
    }
    assert_eq!(row["metadata"]["lora_contract_id"], contract_id);
    assert_eq!(row["metadata"]["lora_target"]["contract_id"], contract_id);
    assert_eq!(row["metadata"]["behavior_class"], "valid_tool_call");
    assert!(
        rows.iter()
            .any(|row| row["metadata"]["behavior_class"] == "no_tool_answer"
                && row["messages"].as_array().is_some_and(|messages| messages
                    .iter()
                    .all(|message| message["role"] != "tool"))),
        "rows={rows:?}"
    );
    let manifest_value = parse_json(
        &fs::read_to_string(&manifest).expect("read manifest"),
        "manifest",
    );
    assert_eq!(manifest_value["contract"]["id"], contract_id);
    assert_eq!(
        manifest_value["contract"]["training_contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        manifest_value["contract"]["training_contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(manifest_value["target"]["contract_id"], contract_id);
    assert_eq!(
        manifest_value["provenance"]["default_split"],
        serde_json::Value::String("train".to_string())
    );
    assert_eq!(
        manifest_value["provenance"]["default_license"],
        serde_json::Value::String("unknown".to_string())
    );
    assert!(
        manifest_value["provenance"]["required_example_metadata"]
            .as_array()
            .expect("manifest provenance fields")
            .iter()
            .any(|field| field == "tool_schema_hash"),
        "manifest provenance={:?}",
        manifest_value["provenance"]
    );
    assert_eq!(manifest_value["serving"]["request_model"], "burin-tools");
    assert_eq!(
        manifest_value["serving"]["adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(
        manifest_value["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(manifest_value["serving"]["contract_id"], contract_id);
    assert_eq!(manifest_value["promotion"]["minimum_trials"], 5);
    assert!(
        manifest_value["promotion"]["gates"]
            .as_array()
            .expect("manifest promotion gates")
            .iter()
            .any(|gate| gate
                .as_str()
                .is_some_and(|text| text.contains("contract-id drift"))),
        "manifest promotion={:?}",
        manifest_value["promotion"]
    );
}

#[test]
fn models_lora_manifest_json_writes_training_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let out = tmp.path().join("adapter.manifest.json");
    let harn = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--out",
            out.to_str().expect("utf8 manifest out path"),
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            &adapter_path,
            "--request-model",
            "burin-tools",
            "--trainer",
            "unsloth_sft",
            "--trainer-version",
            "unsloth-2026.7",
            "--method",
            "qlora",
            "--rank",
            "24",
            "--alpha",
            "48",
            "--training-run-id",
            "run-123",
            "--teacher",
            "dashscope/qwen3-coder-next",
            "--target-metadata",
            "lane=structured",
            "--modules-to-save",
            "embed_tokens,lm_head",
            "--target-modules",
            "q_proj,v_proj",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["producer"], "harn_models_lora_manifest_v1");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["base"]["provider"], "vllm");
    assert_eq!(report["target"]["provider"], "vllm");
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    assert_eq!(report["target"]["request_model"], "burin-tools");
    assert_eq!(report["target"]["harn_tool_format"], "json");
    assert_eq!(
        report["target"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(
        report["target"]["chat_template"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(report["target"]["metadata"]["lane"], "structured");
    assert_eq!(report["training"]["trainer"], "unsloth_sft");
    assert_eq!(report["training"]["method"], "qlora");
    assert_eq!(report["training"]["rank"], 24);
    assert_eq!(report["training"]["alpha"], 48);
    assert_eq!(report["training"]["run_id"], "run-123");
    assert_eq!(
        report["training"]["contract"]["tool_parser_owner"],
        "harn_text_tool_parser"
    );
    assert_eq!(
        report["training"]["precision"]["training_base_precision"],
        "4bit_nf4_or_runtime_equivalent"
    );
    assert_eq!(report["inputs"]["dataset"]["exists"], true);
    assert_eq!(report["inputs"]["dataset"]["kind"], "file");
    assert!(
        report["inputs"]["dataset"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "dataset input={:?}",
        report["inputs"]["dataset"]
    );
    assert_eq!(report["inputs"]["export_manifest"]["exists"], true);
    assert_eq!(
        report["inputs"]["teacher"]["id"],
        "dashscope/qwen3-coder-next"
    );
    assert_eq!(report["artifacts"]["adapter_reference"], adapter_path);
    assert_eq!(report["artifacts"]["local_path"]["kind"], "directory");
    assert!(
        report["artifacts"]["adapter_files"]
            .as_array()
            .expect("adapter files")
            .iter()
            .any(|file| file["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("adapter_config.json"))),
        "artifact files={:?}",
        report["artifacts"]["adapter_files"]
    );
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(report["serving"]["provider"], "vllm");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    let serving_requirements = report["serving"]["serving_requirements"]
        .as_array()
        .expect("serving requirements");
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "parser_owner"
                && requirement["value"] == "harn_text_tool_parser"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "provider_native_tool_parser"
                && requirement["value"] == "disabled_unless_proxy_maps_to_harn_text"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert_eq!(report["promotion"]["minimum_trials"], 5);
    assert_eq!(
        report["promotion"]["evidence_contract"]["schema_version"],
        LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION
    );
    let manifest_probe_cases = report["promotion"]["evidence_contract"]["required_probe_cases"]
        .as_array()
        .expect("manifest required probe cases");
    assert!(
        manifest_probe_cases.iter().any(|probe_case| {
            probe_case["id"] == "serving_concurrency_probe"
                && probe_case["requirement"]
                    == "required_for_adapter_loaded_serving_with_serving_receipt"
        }),
        "manifest required probe cases={manifest_probe_cases:?}"
    );
    assert!(out.is_file(), "manifest file missing");
    assert_eq!(report["contract"]["schema_version"], 3);
    assert_eq!(report["contract"]["training_contract"]["schema_version"], 3);
    assert_eq!(
        report["contract"]["training_contract"]["peft_save_policy"]["schema_version"],
        1
    );

    let manifest_value = parse_json(
        &fs::read_to_string(&out).expect("read manifest"),
        "training manifest",
    );
    let contract_id = report["contract"]["id"].as_str().expect("contract id");
    assert_eq!(manifest_value["contract"]["schema_version"], 3);
    assert_eq!(
        manifest_value["contract"]["training_contract"]["schema_version"],
        3
    );
    assert_eq!(
        manifest_value["contract"]["training_contract"]["peft_save_policy"]["schema_version"],
        1
    );
    assert_eq!(manifest_value["contract"]["id"], contract_id);
    assert_eq!(manifest_value["target"]["contract_id"], contract_id);
    assert_eq!(manifest_value["serving"]["request_model"], "burin-tools");
    assert!(
        manifest_value["serving"]["serving_requirements"]
            .as_array()
            .expect("manifest serving requirements")
            .iter()
            .any(|requirement| requirement["kind"] == "parser_owner"
                && requirement["value"] == "harn_text_tool_parser"),
        "manifest serving={:?}",
        manifest_value["serving"]
    );

    let adapter_with_contract = write_lora_adapter_fixture_with_contract_and_modules(
        Some(contract_id),
        &["lm_head", "embed_tokens"],
    );
    let inspect = run(
        &[
            "models",
            "lora",
            "inspect",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--name",
            "burin-tools",
            "--manifest",
            out.to_str().expect("utf8 manifest path"),
            adapter_with_contract
                .path()
                .to_str()
                .expect("utf8 adapter path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(inspect.exit_code, 0, "inspect stderr={}", inspect.stderr);
    let inspect_value = parse_json(&inspect.stdout, "inspect");
    let inspect_report = success_data(&inspect_value);
    assert_eq!(inspect_report["contract"]["status"], "pass");
    assert_eq!(inspect_report["contract"]["contract_id"], contract_id);
    assert_eq!(
        inspect_report["adapter"]["modules_to_save"],
        serde_json::json!(["embed_tokens", "lm_head"])
    );
}

#[test]
fn models_lora_train_json_writes_dry_run_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let output_dir = tmp.path().join("burin-tools");
    let receipt = tmp.path().join("train.receipt.json");
    let harn = run(
        &[
            "models",
            "lora",
            "train",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--output-dir",
            output_dir.to_str().expect("utf8 output path"),
            "--receipt-out",
            receipt.to_str().expect("utf8 receipt path"),
            "--adapter-name",
            "burin-tools",
            "--request-model",
            "burin-tools",
            "--trainer",
            "unsloth_sft",
            "--trainer-version",
            "unsloth-2026.7",
            "--method",
            "qlora",
            "--rank",
            "24",
            "--alpha",
            "48",
            "--max-seq-length",
            "8192",
            "--target-metadata",
            "lane=structured",
            "--modules-to-save",
            "embed_tokens,lm_head",
            "--json",
            "--",
            "uv",
            "run",
            "python",
            "train.py",
            "config/e4b.yaml",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn lora train");
    let report = success_data(&harn_value);
    assert_eq!(report["producer"], "harn_models_lora_train_v1");
    assert_eq!(report["mode"], "dry_run");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["base"]["provider"], "vllm");
    assert_eq!(report["request"]["effective_tool_format"], "json");
    assert_eq!(
        report["request"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    assert_eq!(report["target"]["request_model"], "burin-tools");
    assert_eq!(report["target"]["metadata"]["lane"], "structured");
    assert_eq!(
        report["target"]["metadata"]["serving_tool_parser_owner"],
        "harn_text_tool_parser"
    );
    assert_eq!(
        report["target"]["metadata"]["serving_adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(report["target"]["metadata"]["serving_provider"], "vllm");
    assert_eq!(report["serving"]["provider"], "vllm");
    assert_eq!(report["training"]["trainer"], "unsloth_sft");
    assert_eq!(report["training"]["trainer_version"], "unsloth-2026.7");
    assert_eq!(report["training"]["rank"], 24);
    assert_eq!(report["training"]["alpha"], 48);
    assert_eq!(report["training"]["max_seq_length"], 8192);
    assert_eq!(
        report["training"]["contract"]["peft_save_policy"]["modules_to_save"],
        serde_json::json!(["embed_tokens", "lm_head"])
    );
    assert_eq!(
        report["training"]["contract"]["peft_save_policy"]["requires_weight_tying_check"],
        true
    );
    assert_eq!(report["inputs"]["dataset"]["exists"], true);
    assert_eq!(report["inputs"]["dataset"]["kind"], "file");
    assert!(
        report["inputs"]["dataset"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "dataset input={:?}",
        report["inputs"]["dataset"]
    );
    assert_eq!(report["backend"]["trainer"], "unsloth_sft");
    assert_eq!(report["backend"]["execute"], false);
    assert_eq!(report["backend"]["status"], "dry_run");
    assert_eq!(
        report["backend"]["argv"]
            .as_array()
            .expect("backend argv")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>(),
        vec!["uv", "run", "python", "train.py", "config/e4b.yaml"]
    );
    assert!(
        report["post_training"]["manifest_command"]
            .as_array()
            .expect("manifest command")
            .iter()
            .any(|arg| arg == "--export-manifest"),
        "post_training={:?}",
        report["post_training"]
    );
    assert!(
        report["post_training"]["manifest_command"]
            .as_array()
            .expect("manifest command")
            .windows(2)
            .any(|pair| pair[0] == "--trainer-version" && pair[1] == "unsloth-2026.7"),
        "post_training={:?}",
        report["post_training"]
    );
    assert!(
        report["post_training"]["manifest_command"]
            .as_array()
            .expect("manifest command")
            .windows(2)
            .any(|pair| pair[0] == "--modules-to-save" && pair[1] == "embed_tokens"),
        "post_training={:?}",
        report["post_training"]
    );
    assert_eq!(
        report["promotion"]["evidence_contract"]["schema_version"],
        LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION
    );
    let train_probe_cases = report["promotion"]["evidence_contract"]["required_probe_cases"]
        .as_array()
        .expect("train required probe cases");
    assert!(
        train_probe_cases
            .iter()
            .any(|probe_case| probe_case["id"] == "unavailable_tool_repair"),
        "train required probe cases={train_probe_cases:?}"
    );

    let receipt_value = parse_json(
        &fs::read_to_string(&receipt).expect("read receipt"),
        "train receipt",
    );
    assert_eq!(receipt_value["producer"], "harn_models_lora_train_v1");
    assert_eq!(receipt_value["backend"]["status"], "dry_run");
    assert!(
        receipt_value["promotion"]["evidence_contract"]["required_probe_cases"]
            .as_array()
            .expect("receipt required probe cases")
            .iter()
            .any(|probe_case| probe_case["id"] == "multi_turn_tool_result_continuation"),
        "receipt promotion={:?}",
        receipt_value["promotion"]
    );
}

#[test]
fn models_lora_promote_json_collects_probe_matrix_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let adapter = write_lora_adapter_fixture();
    let manifest_path = tmp.path().join("adapter.manifest.json");
    let manifest = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            adapter.path().to_str().expect("utf8 adapter path"),
            "--request-model",
            "burin-tools",
            "--trainer-version",
            "trainer-2026.7",
            "--observed-trainer-identity",
            "version=trainer-2026.7",
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "manifest stderr={}", manifest.stderr);

    let probe_root = tmp.path().join("PROMOTION_PROBES");
    let base_probe_root = tmp.path().join("BASE_PROMOTION_PROBES");
    for case_id in [
        "sequential_tool_call",
        "parallel_tool_calls",
        "no_tool_answer",
        "unavailable_tool_repair",
        "multi_turn_tool_result_continuation",
        "serving_concurrency_probe",
    ] {
        write_lora_probe_summary(&probe_root, case_id, true);
        write_lora_probe_summary(&base_probe_root, case_id, case_id == "no_tool_answer");
    }
    let receipt = tmp.path().join("promotion.receipt.json");
    let promoted = run(
        &[
            "models",
            "lora",
            "promote",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--probe-root",
            probe_root.to_str().expect("utf8 probe root"),
            "--base-probe-root",
            base_probe_root.to_str().expect("utf8 base probe root"),
            "--out",
            receipt.to_str().expect("utf8 receipt path"),
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(promoted.exit_code, 0, "promote stderr={}", promoted.stderr);
    let promoted_value = parse_json(&promoted.stdout, "promote");
    let report = success_data(&promoted_value);
    assert_eq!(report["producer"], "harn_models_lora_promote_v2");
    assert_eq!(report["receipt_kind"], "promotion_evidence_bundle_receipt");
    assert_eq!(report["ok"], true);
    assert_eq!(
        report["contract"]["eval_dataset"],
        dataset.display().to_string()
    );
    assert_eq!(report["contract"]["trainer_identity"]["status"], "matched");
    assert_eq!(report["contract"]["trainer_identity"]["promotable"], true);
    assert_eq!(report["totals"]["required_cases"], 6);
    assert_eq!(report["totals"]["evidence_records"], 12);
    assert_eq!(report["totals"]["adapter_passed"], 6);
    assert_eq!(report["totals"]["adapter_failed"], 0);
    assert_eq!(report["totals"]["base_present"], 6);
    assert_eq!(report["totals"]["baseline_failed"], 5);
    assert_eq!(report["totals"]["missing"], 0);
    let cases = report["cases"].as_array().expect("promotion cases");
    assert!(
        cases
            .iter()
            .any(|case| case["case_id"] == "sequential_tool_call"
                && case["route_role"] == "adapter"
                && case["status"] == "pass"
                && case["summary_sha256"]
                    .as_str()
                    .is_some_and(|sha| sha.starts_with("sha256:"))
                && case["per_case_sha256"]
                    .as_str()
                    .is_some_and(|sha| sha.starts_with("sha256:"))
                && case["summary_path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("sequential_tool_call/summary.json"))),
        "cases={cases:?}"
    );
    assert!(
        cases
            .iter()
            .any(|case| case["case_id"] == "sequential_tool_call"
                && case["route_role"] == "base"
                && case["status"] == "baseline_fail"),
        "cases={cases:?}"
    );
    let receipt_value = parse_json(
        &fs::read_to_string(&receipt).expect("read promotion receipt"),
        "promotion receipt",
    );
    assert_eq!(receipt_value["producer"], "harn_models_lora_promote_v2");
    assert_eq!(receipt_value["totals"]["adapter_passed"], 6);
}

#[test]
fn models_lora_promote_check_rejects_single_trial_probe_receipts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let adapter = write_lora_adapter_fixture();
    let manifest_path = tmp.path().join("adapter.manifest.json");
    let manifest = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            adapter.path().to_str().expect("utf8 adapter path"),
            "--request-model",
            "burin-tools",
            "--trainer-version",
            "trainer-2026.7",
            "--observed-trainer-identity",
            "version=trainer-2026.7",
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "manifest stderr={}", manifest.stderr);

    let probe_root = tmp.path().join("PROMOTION_PROBES");
    let base_probe_root = tmp.path().join("BASE_PROMOTION_PROBES");
    for case_id in [
        "sequential_tool_call",
        "parallel_tool_calls",
        "no_tool_answer",
        "unavailable_tool_repair",
        "multi_turn_tool_result_continuation",
        "serving_concurrency_probe",
    ] {
        write_lora_probe_summary_with_trials(&probe_root, case_id, true, 1);
        write_lora_probe_summary_with_trials(&base_probe_root, case_id, true, 1);
    }
    let promoted = run(
        &[
            "models",
            "lora",
            "promote",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--probe-root",
            probe_root.to_str().expect("utf8 probe root"),
            "--base-probe-root",
            base_probe_root.to_str().expect("utf8 base probe root"),
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(promoted.exit_code, 1, "promote stdout={}", promoted.stdout);
    let promoted_value = parse_json(&promoted.stdout, "failed promote");
    assert_eq!(promoted_value["ok"], serde_json::Value::Bool(false));
    let details = &promoted_value["error"]["details"];
    assert_eq!(details["producer"], "harn_models_lora_promote_v2");
    assert!(
        details["errors"]
            .as_array()
            .expect("promotion errors")
            .iter()
            .any(|error| error
                .as_str()
                .is_some_and(|text| text.contains("has only 1 trials; required 5"))),
        "details={details:?}"
    );
}

#[test]
fn models_lora_promote_check_fails_when_probe_matrix_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let manifest_path = tmp.path().join("adapter.manifest.json");
    let manifest = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--adapter-name",
            "burin-tools",
            "--request-model",
            "burin-tools",
            "--trainer-version",
            "trainer-2026.7",
            "--observed-trainer-identity",
            "version=trainer-2026.7",
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "manifest stderr={}", manifest.stderr);

    let probe_root = tmp.path().join("empty-probes");
    fs::create_dir_all(&probe_root).expect("probe root");
    let promoted = run(
        &[
            "models",
            "lora",
            "promote",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--probe-root",
            probe_root.to_str().expect("utf8 probe root"),
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(promoted.exit_code, 1, "promote stderr={}", promoted.stderr);
    let promoted_value = parse_json(&promoted.stdout, "failed promote");
    assert_eq!(promoted_value["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        promoted_value["error"]["code"],
        "lora_promotion_probe_matrix_failed"
    );
    let report = &promoted_value["error"]["details"];
    assert_eq!(report["producer"], "harn_models_lora_promote_v2");
    assert_eq!(report["totals"]["missing"], 12);
    assert!(
        report["errors"]
            .as_array()
            .expect("promotion errors")
            .iter()
            .any(|error| error
                .as_str()
                .is_some_and(|text| text.contains("sequential_tool_call adapter missing"))),
        "report={report:?}"
    );
}

#[test]
fn models_lora_manifest_human_text_reports_contract() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            &adapter_path,
            "--request-model",
            "burin-tools",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA manifest for burin-tools on gemma-4-e4b-it via vllm",
        "producer: harn_models_lora_manifest_v1",
        "tool format: json (requested json)",
        "dataset format: harn_text_tool_calls_json_fences",
        "chat template: harn_text_tool_calls_json_fences",
        "contract: sha256:",
        "request model: burin-tools",
        "trainer: external_sft_trainer",
        "training: qlora + peft_lora",
        "assistant mask: require_chat_template_generation_masks",
        "parser owner: harn_text_tool_parser",
        "tool catalog policy: full_schema",
        "adapter reference:",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "promotion minimum trials: 5",
        "required probe cases:",
        "unavailable_tool_repair [always]",
        "serving_concurrency_probe [required_for_adapter_loaded_serving_with_serving_receipt]",
        "harn eval tool-calls --planner burin-tools --tool-format json",
        "warnings:",
        "- no --out supplied; manifest report was not written to disk",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_export_json_structures_grouped_tool_results() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["stats"]["records"].as_u64(), Some(6));
    assert_eq!(report["stats"]["emitted"].as_u64(), Some(5));
    assert_eq!(report["stats"]["tool_calls"].as_u64(), Some(5));
    assert_eq!(report["stats"]["tool_results"].as_u64(), Some(5));

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let row = row_text
        .lines()
        .map(|line| parse_json(line, "export row"))
        .find(|row| row["id"] == "parallel-results")
        .expect("parallel row");
    let messages = row["messages"].as_array().expect("messages array");
    let tool_messages = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2, "messages={messages:?}");
    assert_eq!(tool_messages[0]["name"], "read");
    assert_eq!(tool_messages[0]["tool_call_id"], "call_2_1");
    assert_eq!(tool_messages[0]["content"], "pub fn add() {}");
    assert_eq!(tool_messages[1]["name"], "run");
    assert_eq!(tool_messages[1]["tool_call_id"], "call_2_2");
    assert_eq!(tool_messages[1]["content"], "1 passed");
}

#[test]
fn models_lora_export_text_preserves_declared_no_tool_completion() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("text.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(
        report["request"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["no_tool_answer"].as_u64(),
        Some(1)
    );
    assert_eq!(
        report["stats"]["behavior_strata"]["emitted"]["unavailable_tool_repair"].as_u64(),
        Some(1)
    );

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let rows = row_text
        .lines()
        .map(|line| parse_json(line, "export row"))
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|row| row["metadata"]["behavior_class"] == "no_tool_answer"
                && row["assistant_tool_text"]
                    .as_str()
                    .is_some_and(|text| !text.contains("<tool_call>"))),
        "rows={rows:?}"
    );
}

#[test]
fn models_lora_export_rejects_generic_placeholder_after_tool_calls() {
    let corpus = write_lora_generic_placeholder_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let errors = harn_value["error"]["details"]["errors"]
        .as_array()
        .expect("errors");
    assert!(
        errors
            .iter()
            .any(|error| error.as_str().is_some_and(|text| text
                .contains("assistant tool calls must be followed by typed tool-result messages"))),
        "errors={errors:?}"
    );
}

// ────────────────────────────────────────────────────────────────────────
