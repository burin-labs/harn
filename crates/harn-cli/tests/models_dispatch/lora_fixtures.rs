use std::fs;

pub(super) fn write_lora_adapter_fixture() -> tempfile::TempDir {
    write_lora_adapter_fixture_with_contract(None)
}

pub(super) fn write_lora_adapter_fixture_with_contract(
    contract_id: Option<&str>,
) -> tempfile::TempDir {
    write_lora_adapter_fixture_with_contract_and_modules(contract_id, &[])
}

pub(super) fn write_lora_adapter_fixture_with_contract_and_modules(
    contract_id: Option<&str>,
    modules_to_save: &[&str],
) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("adapter_model.safetensors"), b"stub").expect("adapter weights");
    let mut config = serde_json::json!({
        "peft_type": "LORA",
        "base_model_name_or_path": "google/gemma-4-e4b-it",
        "task_type": "CAUSAL_LM",
        "r": 16,
        "lora_alpha": 32,
        "target_modules": ["q_proj", "v_proj"]
    });
    if let Some(contract_id) = contract_id {
        config["harn_lora_contract_id"] = serde_json::Value::String(contract_id.to_string());
    }
    if !modules_to_save.is_empty() {
        config["modules_to_save"] = serde_json::json!(modules_to_save);
    }
    fs::write(
        tmp.path().join("adapter_config.json"),
        serde_json::to_string_pretty(&config).expect("adapter config JSON"),
    )
    .expect("adapter config");
    tmp
}

pub(super) fn write_lora_manifest_fixture(
    root: &std::path::Path,
    contract_id: &str,
) -> std::path::PathBuf {
    let path = root.join("export.manifest.json");
    let manifest = serde_json::json!({
        "exporter": "harn_models_lora_export_v1",
        "dataset_format": "harn_text_tool_calls_json_fences",
        "target": {
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "adapter_name": "burin-tools",
            "harn_tool_format": "json",
            "contract_id": contract_id,
            "tool_catalog": {
                "schema_version": 1,
                "policy": "full_schema",
                "catalog_id": null,
                "catalog_hash": null,
                "training_catalog": "full_json_schema",
                "inference_catalog": "full_json_schema",
                "schema_columns_required": true,
                "prompt_catalog_requirement": "include full tool schemas at inference",
                "notes": [],
                "promotion_gates": []
            }
        },
        "contract": {
            "schema_version": 3,
            "id": contract_id,
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "harn_tool_format": "json",
            "dataset_format": "harn_text_tool_calls_json_fences",
            "chat_template": "harn_text_tool_calls_json_fences",
            "target_modules": {
                "policy": "explicit",
                "modules": ["q_proj", "v_proj"]
            },
            "training_contract": {
                "schema_version": 3,
                "loss_scope": "assistant_tool_calls",
                "assistant_mask_policy": "require_chat_template_generation_masks",
                "packing_policy": "disabled_unless_boundary_aware_tool_pack_pairs",
                "tool_parser_owner": "harn_text_tool_parser",
                "dataset_format": "harn_text_tool_calls_json_fences",
                "dataset_split_policy": "train_tune_holdout_disjoint_no_eval_holdout_training",
                "tool_catalog": {
                    "schema_version": 1,
                    "policy": "full_schema",
                    "catalog_id": null,
                    "catalog_hash": null,
                    "training_catalog": "full_json_schema",
                    "inference_catalog": "full_json_schema",
                    "schema_columns_required": true,
                    "prompt_catalog_requirement": "include full tool schemas at inference",
                    "notes": [],
                    "promotion_gates": []
                },
                "peft_save_policy": {
                    "schema_version": 1,
                    "modules_to_save": [],
                    "save_embedding_layers": "disabled_unless_tokenizer_vocab_changed",
                    "tied_embedding_policy": "no_embedding_or_lm_head_adapter_weights_expected",
                    "requires_weight_tying_check": false,
                    "notes": []
                },
                "required_example_metadata": []
            }
        },
        "serving": {
            "request_model": "burin-tools",
            "adapter_name": "burin-tools",
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "adapter_binding": "runtime_lora_adapter",
            "lora_module_value_format": "json_with_base_model",
            "tool_format": "json",
            "dataset_format": "harn_text_tool_calls_json_fences",
            "contract_id": contract_id,
            "tool_catalog": {
                "schema_version": 1,
                "policy": "full_schema",
                "catalog_id": null,
                "catalog_hash": null,
                "training_catalog": "full_json_schema",
                "inference_catalog": "full_json_schema",
                "schema_columns_required": true,
                "prompt_catalog_requirement": "include full tool schemas at inference",
                "notes": [],
                "promotion_gates": []
            }
        },
        "promotion": {
            "minimum_trials": 5,
            "gates": []
        }
    });
    fs::write(
        &path,
        serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    )
    .expect("write manifest");
    path
}

pub(super) fn write_lora_probe_summary(root: &std::path::Path, case_id: &str, passed: bool) {
    let case_dir = root.join(case_id);
    fs::create_dir_all(&case_dir).expect("probe case dir");
    let summary = serde_json::json!({
        "total_cases": 1,
        "passed_cases": i32::from(passed),
        "pass_rate": if passed { 1.0 } else { 0.0 },
        "total_cost_usd": 0.01,
        "cases": [
            {
                "id": case_id,
                "passed": passed,
                "reason": if passed { "ok" } else { "failed" }
            }
        ]
    });
    fs::write(
        case_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary).expect("summary JSON"),
    )
    .expect("write summary");
    fs::write(
        case_dir.join("per_case.jsonl"),
        format!("{}\n", serde_json::json!({"id": case_id, "passed": passed})),
    )
    .expect("write per-case");
}

pub(super) fn write_lora_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "tiny-read",
        "language": "rust",
        "task_type": "explain",
        "eval_name": "tiny-read",
        "model": "manual",
        "metadata": {
            "verification": "PASS"
        },
        "messages": [
            {
                "role": "system",
                "content": "Available tools: read, run\n- edit(action, path, content)\ndeclare function read(args: { path: string }): string;"
            },
            {
                "role": "user",
                "content": "Read src/lib.rs and summarize it."
            },
            {
                "role": "assistant",
                "content": "<assistant_prose>\nI will inspect the file.\n</assistant_prose>\n\n<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[result of read] pub fn add(a: i32, b: i32) -> i32 { a + b } [end of read result]"
            },
            {
                "role": "assistant",
                "content": "<assistant_prose>\nThe file defines an add helper.\n</assistant_prose>\n<done>##DONE##</done>"
            }
        ]
    });
    let context_row = serde_json::json!({
        "id": "source-context",
        "eval_name": "source-context",
        "source_context": {
            "notes": "metadata-only rows are not training examples"
        },
        "messages": []
    });
    fs::write(
        tmp.path().join("burin-tool-calling-corpus.jsonl"),
        serde_json::to_string(&record).expect("serialize record")
            + "\n"
            + &serde_json::to_string(&context_row).expect("serialize context row")
            + "\n",
    )
    .expect("write corpus");
    tmp
}

pub(super) fn write_lora_grouped_result_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "grouped-results",
        "language": "rust",
        "task_type": "test",
        "eval_name": "grouped-results",
        "model": "manual",
        "metadata": {
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {
                "role": "system",
                "content": "Available tools: read, run"
            },
            {
                "role": "user",
                "content": "Inspect and test src/lib.rs."
            },
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n\n<tool_call>\n{\"name\":\"run\",\"arguments\":{\"command\":\"cargo test\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[result of read src/lib.rs]\npub fn add() {}\n[end of read result]\n\n[result of run]\n1 passed\n[end of run result]"
            }
        ]
    });
    fs::write(
        tmp.path().join("burin-tool-calling-corpus.jsonl"),
        serde_json::to_string(&record).expect("serialize record") + "\n",
    )
    .expect("write corpus");
    tmp
}
