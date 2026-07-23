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
    write_lora_probe_summary_with_trials(root, case_id, passed, 5);
}

pub(super) fn write_lora_probe_summary_with_trials(
    root: &std::path::Path,
    case_id: &str,
    passed: bool,
    trials: usize,
) {
    let case_dir = root.join(case_id);
    fs::create_dir_all(&case_dir).expect("probe case dir");
    let cases = (0..trials)
        .map(|_| {
            serde_json::json!({
                "id": case_id,
                "passed": passed,
                "reason": if passed { "ok" } else { "failed" }
            })
        })
        .collect::<Vec<_>>();
    let summary = serde_json::json!({
        "total_cases": trials,
        "passed_cases": if passed { trials } else { 0 },
        "pass_rate": if passed { 1.0 } else { 0.0 },
        "total_cost_usd": 0.01,
        "cases": cases
    });
    fs::write(
        case_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary).expect("summary JSON"),
    )
    .expect("write summary");
    fs::write(
        case_dir.join("per_case.jsonl"),
        (0..trials)
            .map(|_| serde_json::json!({"id": case_id, "passed": passed}).to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .expect("write per-case");
}

pub(super) fn write_lora_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sequential = serde_json::json!({
        "id": "tiny-read",
        "language": "rust",
        "task_type": "explain",
        "eval_name": "tiny-read",
        "model": "manual",
        "metadata": {
            "behavior_class": "valid_tool_call",
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
    let parallel = serde_json::json!({
        "id": "parallel-results",
        "language": "rust",
        "task_type": "test",
        "eval_name": "parallel-results",
        "model": "manual",
        "metadata": {
            "behavior_classes": ["valid_tool_call", "parallel_tool_call"],
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {"role": "system", "content": "Available tools: read, run"},
            {"role": "user", "content": "Inspect and test src/lib.rs."},
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n\n<tool_call>\n{\"name\":\"run\",\"arguments\":{\"command\":\"cargo test\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[result of read]\npub fn add() {}\n[end of read result]\n\n[result of run]\n1 passed\n[end of run result]"
            },
            {
                "role": "assistant",
                "content": "The file was inspected and tests pass.\n<done>##DONE##</done>"
            }
        ]
    });
    let no_tool = serde_json::json!({
        "id": "direct-answer",
        "language": "rust",
        "task_type": "question",
        "eval_name": "direct-answer",
        "model": "manual",
        "metadata": {
            "behavior_class": "no_tool_answer",
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {"role": "system", "content": "Available tools: read, run"},
            {"role": "user", "content": "Reply with the number of letters in rust."},
            {"role": "assistant", "content": "The word rust has 4 letters.\n<done>##DONE##</done>"}
        ]
    });
    let unavailable = serde_json::json!({
        "id": "unavailable-tool",
        "language": "rust",
        "task_type": "repair",
        "eval_name": "unavailable-tool",
        "model": "manual",
        "metadata": {
            "behavior_class": "unavailable_tool_repair",
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {"role": "system", "content": "Available tools: read, run"},
            {"role": "user", "content": "Use web_search to check docs."},
            {"role": "assistant", "content": "web_search is not available in this session; I will use the workspace tools instead if needed.\n<done>##DONE##</done>"}
        ]
    });
    let continuation = serde_json::json!({
        "id": "multi-turn",
        "language": "rust",
        "task_type": "test",
        "eval_name": "multi-turn",
        "model": "manual",
        "metadata": {
            "behavior_classes": ["valid_tool_call", "multi_turn_continuation"],
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {"role": "system", "content": "Available tools: read, run"},
            {"role": "user", "content": "Inspect then run tests."},
            {"role": "assistant", "content": "<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n"},
            {"role": "user", "content": "[result of read]\npub fn add() {}\n[end of read result]"},
            {"role": "assistant", "content": "<tool_call>\n{\"name\":\"run\",\"arguments\":{\"command\":\"cargo test\"}}\n</tool_call>\n"},
            {"role": "user", "content": "[result of run]\n1 passed\n[end of run result]"},
            {"role": "assistant", "content": "Tests pass.\n<done>##DONE##</done>"}
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
        serde_json::to_string(&sequential).expect("serialize sequential")
            + "\n"
            + &serde_json::to_string(&parallel).expect("serialize parallel")
            + "\n"
            + &serde_json::to_string(&no_tool).expect("serialize no-tool")
            + "\n"
            + &serde_json::to_string(&unavailable).expect("serialize unavailable")
            + "\n"
            + &serde_json::to_string(&continuation).expect("serialize continuation")
            + "\n"
            + &serde_json::to_string(&context_row).expect("serialize context row")
            + "\n",
    )
    .expect("write corpus");
    tmp
}

pub(super) fn write_lora_generic_placeholder_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "generic-placeholder-results",
        "language": "rust",
        "task_type": "test",
        "eval_name": "generic-placeholder-results",
        "model": "manual",
        "metadata": {
            "behavior_classes": ["valid_tool_call", "parallel_tool_call"],
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {"role": "system", "content": "Available tools: read, run"},
            {"role": "user", "content": "Inspect and test src/lib.rs."},
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n\n<tool_call>\n{\"name\":\"run\",\"arguments\":{\"command\":\"cargo test\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[tool results applied; continuing]"
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
