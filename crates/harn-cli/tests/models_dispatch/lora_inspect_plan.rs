use super::lora_fixtures::{
    write_lora_adapter_fixture, write_lora_adapter_fixture_with_contract,
    write_lora_manifest_fixture,
};
use super::support::{parse_json, run, success_data, LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION};

// - models lora inspect ---------------------------------------------------

#[test]
fn models_lora_inspect_human_text_includes_launch_hint() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
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
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "burin-tools -> gemma-4-e4b-it via vllm",
        "base match: same basename",
        "tool format: json",
        "native tools: no, preferred: unset",
        "catalog LoRA launch flags: yes",
        "LoRA module format: json_with_base_model",
        "catalog LoRA rank flag: yes",
        "max LoRA rank: 16",
        "harn local launch local-gemma4-e4b --provider vllm",
        "--model-source google/gemma-4-e4b-it",
        "--lora-adapter burin-tools=",
        "--max-lora-rank 16",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_canonicalizes_local_vllm_provider_alias() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
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
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "burin-tools -> gemma-4-e4b-it via vllm",
        "catalog LoRA launch flags: yes",
        "LoRA module format: json_with_base_model",
        "harn local launch local-gemma4-e4b --provider vllm",
        "--lora-adapter burin-tools=",
        "--max-lora-rank 16",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_human_text_omits_launch_hint_when_provider_cannot_launch_lora() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
        &[
            "models",
            "lora",
            "inspect",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "openai",
            "--name",
            "burin-tools",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "burin-tools -> gemma-4-e4b-it via openai",
        "catalog LoRA launch flags: no",
        "catalog LoRA rank flag: no",
        "warning: provider openai does not declare local-runtime LoRA launch flags",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
    for fragment in ["  launch:", "harn local launch", "--lora-adapter"] {
        assert!(
            !harn.stdout.contains(fragment),
            "harn stdout unexpectedly contained {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_json_shape_is_stable() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
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
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["base"]["selector"], "local-gemma4-e4b");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["base"]["provider"], "vllm");
    assert_eq!(report["base"]["tool_format"], "json");
    assert_eq!(report["adapter"]["name"], "burin-tools");
    assert_eq!(report["adapter"]["peft_type"], "LORA");
    assert_eq!(report["compatibility"]["base_model_match"], "suffix");
    assert_eq!(
        report["compatibility"]["provider_supports_lora_launch"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        report["compatibility"]["provider_supports_lora_max_rank"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        report["compatibility"]["provider_lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["tool_calling"]["native_tools"], false);
    assert_eq!(report["serving"]["request_model"], "burin-tools");
    assert_eq!(report["serving"]["base_model"], "gemma-4-e4b-it");
    assert_eq!(report["serving"]["provider"], "vllm");
    assert_eq!(report["serving"]["tool_format"], "json");
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
                && requirement["name"] == "tool_call_parser"
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
    assert_eq!(report["launch"]["request_model"], "burin-tools");
    assert_eq!(report["launch"]["max_lora_rank"].as_u64(), Some(16));
    let launch = report["launch"]["harn_local_launch"]
        .as_array()
        .expect("launch argv");
    assert!(
        launch.iter().any(|arg| arg == "--lora-adapter"),
        "launch argv={launch:?}"
    );
    assert!(
        launch
            .windows(2)
            .any(|pair| pair[0] == "--max-lora-rank" && pair[1] == "16"),
        "launch argv={launch:?}"
    );
}

#[test]
fn models_lora_inspect_manifest_contract_reports_match() {
    let contract_id = "sha256:fixture-contract";
    let adapter = write_lora_adapter_fixture_with_contract(Some(contract_id));
    let manifest = write_lora_manifest_fixture(adapter.path(), contract_id);
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
    let harn = run(
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
            &manifest_path,
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    let contract = report["contract"].as_object().expect("contract report");
    assert_eq!(contract["status"], "pass");
    assert_eq!(contract["contract_id"], contract_id);
    assert_eq!(contract["adapter_contract_id"], contract_id);
    assert_eq!(contract["base_model_match"], "exact");
    assert_eq!(contract["provider_matches"], true);
    assert_eq!(contract["tool_format_matches"], true);
    assert_eq!(contract["adapter_name_matches"], true);
    assert_eq!(
        contract["manifest"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert!(contract["warnings"]
        .as_array()
        .expect("warnings")
        .is_empty());
}

#[test]
fn models_lora_inspect_manifest_human_text_reports_contract_route() {
    let contract_id = "sha256:fixture-contract";
    let adapter = write_lora_adapter_fixture_with_contract(Some(contract_id));
    let manifest = write_lora_manifest_fixture(adapter.path(), contract_id);
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
    let harn = run(
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
            &manifest_path,
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "contract: sha256:fixture-contract",
        "contract status: pass",
        "adapter contract id: sha256:fixture-contract",
        "contract route: base=exact provider=match tool_format=match adapter=match",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_require_contract_id_fails_when_adapter_omits_it() {
    let adapter = write_lora_adapter_fixture();
    let manifest = write_lora_manifest_fixture(adapter.path(), "sha256:fixture-contract");
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
    let harn = run(
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
            &manifest_path,
            "--require-contract-id",
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["contract"]["status"], "fail");
    assert_eq!(
        details["contract"]["adapter_contract_id"],
        serde_json::Value::Null
    );
    let warnings = details["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("LoRA contract missing"))),
        "warnings={warnings:?}"
    );
}

#[test]
fn models_lora_plan_human_text_includes_recipe() {
    let harn = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "local-vllm",
            "--tool-format",
            "auto",
            "--corpus",
            "./lora-corpus",
            "--teacher",
            "dashscope/qwen3-coder-next",
            "--trainer",
            "unsloth_sft",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA plan for gemma-4-e4b-it via vllm",
        "tool format: json (requested auto)",
        "training: qlora + peft_lora",
        "trainer: unsloth_sft",
        "LoRA hparams: rank=16 alpha=32 dropout=0.05",
        "target modules: all-linear",
        "training base precision: 4bit_nf4_or_runtime_equivalent",
        "training compute precision: bf16_when_supported_else_fp16",
        "adapter precision: bf16_or_fp16_lora_weights",
        "serving base precision: same_quantization_family_as_training_or_revalidate",
        "precision gates:",
        "assistant mask: require_chat_template_generation_masks",
        "parser owner: harn_text_tool_parser",
        "split policy: train_tune_holdout_disjoint_no_eval_holdout_training",
        "template: harn_text_tool_calls_json_fences",
        "template source: Harn text tool-call parser using JSON object bodies",
        "dataset format: harn_text_tool_calls_json_fences",
        "corpus: ./lora-corpus",
        "corpus strategy: refresh (requested auto)",
        "teacher: dashscope/qwen3-coder-next via dashscope",
        "provenance manifest:",
        "hard negatives:",
        "corpus gates:",
        "trainer contract:",
        "set assistant_only_loss=true",
        "Harn remains the parser at inference",
        "use Unsloth only as the trainer backend",
        "record torch/CUDA, tokenizer class, and chat-template hash",
        "minimum trials: 5",
        "comparison baseline: same base model, provider, tool format, prompt template, and tool schemas without the adapter",
        "required metrics:",
        "Harn text parser acceptance rate",
        "required probe cases:",
        "sequential_tool_call [always]",
        "parallel_tool_calls [required_when_route_supports_parallel_tool_calls_else_not_applicable_receipt]",
        "multi_turn_tool_result_continuation [always]",
        "serving_concurrency_probe [required_for_adapter_loaded_serving_else_not_applicable_receipt]",
        "require a positive paired lift before promotion; inconclusive movement stays experimental",
        "require zero contract-id drift between export manifest, adapter metadata, and served route",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "serving notes:",
        "serve the adapter as a text-channel route: Harn owns tool-call parsing for this plan",
        "keep provider-native tool parsers disabled unless the proxy maps them back to Harn text tool calls",
        "promotion gates:",
        "harn models lora preflight --base local-gemma4-e4b --provider vllm --tool-format json --corpus ./lora-corpus --source-tool-format json --check",
        "harn models lora export --base local-gemma4-e4b --provider vllm --tool-format json --corpus ./lora-corpus --out ADAPTER_DATASET.jsonl --manifest ADAPTER_DATASET.manifest.json --adapter-name ADAPTER_NAME --chat-template harn_text_tool_calls_json_fences",
        "harn models lora train --base local-gemma4-e4b --provider vllm --tool-format json --dataset ADAPTER_DATASET.jsonl --export-manifest ADAPTER_DATASET.manifest.json --output-dir ADAPTER_OUTPUT_DIR --receipt-out ADAPTER_OUTPUT_DIR/train.receipt.json",
        "harn eval tool-calls --planner ADAPTER_MODEL --tool-format json --dataset ./lora-corpus",
        "harn models lora promote --manifest ADAPTER_OUTPUT_DIR/adapter.manifest.json --probe-root PROMOTION_PROBES --out ADAPTER_OUTPUT_DIR/promotion.receipt.json --check",
        "harn models lora inspect --base local-gemma4-e4b --provider vllm --name ADAPTER_NAME ADAPTER_PATH_OR_REPO",
        "harn local launch local-gemma4-e4b --provider vllm --model-source gemma-4-e4b-it",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_plan_json_shape_is_stable() {
    let harn = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--method",
            "lora",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["request"]["method"], "lora");
    assert_eq!(report["request"]["requested_tool_format"], "native");
    assert_eq!(report["request"]["effective_tool_format"], "native");
    assert_eq!(report["training"]["adapter_type"], "peft_lora");
    assert_eq!(report["training"]["trainer"], "external_sft_trainer");
    assert_eq!(report["training"]["rank"], 16);
    assert_eq!(report["training"]["alpha"], 32);
    assert_eq!(report["training"]["dropout"], 0.05);
    assert_eq!(report["training"]["quantization"], "base_model_precision");
    assert_eq!(
        report["training"]["target_modules"]["policy"],
        "route_default"
    );
    let target_modules = report["training"]["target_modules"]["modules"]
        .as_array()
        .expect("target modules");
    assert_eq!(
        target_modules,
        &[
            serde_json::Value::from("q_proj"),
            serde_json::Value::from("k_proj"),
            serde_json::Value::from("v_proj"),
            serde_json::Value::from("o_proj"),
            serde_json::Value::from("gate_proj"),
            serde_json::Value::from("up_proj"),
            serde_json::Value::from("down_proj"),
        ]
    );
    assert_eq!(report["precision"]["schema_version"], 1);
    assert_eq!(
        report["precision"]["training_base_precision"],
        "base_model_precision"
    );
    assert_eq!(
        report["precision"]["serving_base_precision"],
        "same_base_model_precision_as_training_or_revalidate"
    );
    let precision_gates = report["precision"]["promotion_gates"]
        .as_array()
        .expect("precision promotion gates");
    assert!(
        precision_gates.iter().any(|gate| gate
            .as_str()
            .is_some_and(|text| text.contains("compute dtype"))),
        "precision gates={precision_gates:?}"
    );
    assert_eq!(report["training"]["contract"]["schema_version"], 3);
    assert_eq!(
        report["training"]["contract"]["tool_catalog"]["policy"],
        "full_schema"
    );
    assert_eq!(
        report["training"]["contract"]["tool_catalog"]["inference_catalog"],
        "full_json_schema"
    );
    let tool_catalog_gates = report["training"]["contract"]["tool_catalog"]["promotion_gates"]
        .as_array()
        .expect("tool catalog gates");
    assert!(
        tool_catalog_gates.iter().any(|gate| gate
            .as_str()
            .is_some_and(|text| text.contains("catalog policy"))),
        "tool catalog gates={tool_catalog_gates:?}"
    );
    assert_eq!(
        report["training"]["contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        report["training"]["contract"]["packing_policy"],
        "disabled_unless_boundary_aware_tool_pack_pairs"
    );
    assert_eq!(
        report["training"]["contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(
        report["training"]["contract"]["dataset_split_policy"],
        "train_tune_holdout_disjoint_no_eval_holdout_training"
    );
    assert_eq!(
        report["training"]["contract"]["peft_save_policy"]["schema_version"],
        1
    );
    assert_eq!(
        report["training"]["contract"]["peft_save_policy"]["modules_to_save"],
        serde_json::json!([])
    );
    let trainer_contract = report["training"]["trainer_contract"]
        .as_array()
        .expect("trainer contract");
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("assistant_only_loss=true"))),
        "trainer contract={trainer_contract:?}"
    );
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("messages plus a tools column"))),
        "trainer contract={trainer_contract:?}"
    );
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("modules_to_save=[]"))),
        "trainer contract={trainer_contract:?}"
    );
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("external trainers must reproduce"))),
        "trainer contract={trainer_contract:?}"
    );
    assert_eq!(report["data"]["dataset_format"], "messages_with_tool_calls");
    assert_eq!(report["request"]["requested_corpus_strategy"], "auto");
    assert_eq!(report["request"]["effective_corpus_strategy"], "audit-only");
    assert!(report["request"]["teacher"].is_null());
    assert_eq!(report["corpus_refresh"]["strategy"], "audit-only");
    assert_eq!(report["corpus_refresh"]["teacher_required"], false);
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["serving"]["request_model"], "ADAPTER_MODEL");
    assert_eq!(report["serving"]["adapter_name"], "ADAPTER_NAME");
    assert_eq!(report["serving"]["tool_format"], "native");
    assert_eq!(
        report["serving"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(report["serving"]["tool_catalog"]["policy"], "full_schema");
    let export = report["launch"]["export_command"]
        .as_array()
        .expect("export argv");
    let preflight = report["launch"]["preflight_command"]
        .as_array()
        .expect("preflight argv");
    assert!(
        preflight
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "preflight argv={preflight:?}"
    );
    assert!(
        preflight
            .windows(2)
            .any(|pair| pair[0] == "--source-tool-format" && pair[1] == "json"),
        "preflight argv={preflight:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "export argv={export:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--corpus" && pair[1] == "CORPUS_JSONL_OR_DIR"),
        "export argv={export:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--chat-template" && pair[1] == "gemma4_native_function_calling"),
        "export argv={export:?}"
    );
    assert!(
        export.windows(2).any(|pair| pair[0] == "--target-metadata"
            && pair[1] == "training_base_precision=base_model_precision"),
        "export argv={export:?}"
    );
    assert!(
        export.windows(2).any(|pair| pair[0] == "--target-metadata"
            && pair[1]
                == "serving_base_precision=same_base_model_precision_as_training_or_revalidate"),
        "export argv={export:?}"
    );
    let train = report["launch"]["train_command"]
        .as_array()
        .expect("train argv");
    assert!(
        train
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "train argv={train:?}"
    );
    assert!(
        train
            .windows(2)
            .any(|pair| pair[0] == "--receipt-out"
                && pair[1] == "ADAPTER_OUTPUT_DIR/train.receipt.json"),
        "train argv={train:?}"
    );
    let promote = report["launch"]["promote_command"]
        .as_array()
        .expect("promote argv");
    assert!(
        promote
            .windows(2)
            .any(|pair| pair[0] == "--manifest"
                && pair[1] == "ADAPTER_OUTPUT_DIR/adapter.manifest.json"),
        "promote argv={promote:?}"
    );
    assert!(
        promote.windows(2).any(
            |pair| pair[0] == "--out" && pair[1] == "ADAPTER_OUTPUT_DIR/promotion.receipt.json"
        ),
        "promote argv={promote:?}"
    );
    assert!(
        promote.iter().any(|arg| arg == "--check"),
        "promote argv={promote:?}"
    );
    let serving_notes = report["serving"]["runtime_notes"]
        .as_array()
        .expect("serving runtime notes");
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("schema-constrained or strict tool calling"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("--enable-auto-tool-choice"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("Gemma 4 native routes"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("serialize Gemma 4 native-tool validation traffic"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note.as_str().is_some_and(
            |text| text.contains("gemma4 tool-call parser and chat-template revision")
        )),
        "serving notes={serving_notes:?}"
    );
    let serving_requirements = report["serving"]["serving_requirements"]
        .as_array()
        .expect("serving requirements");
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "server_flag"
                && requirement["name"] == "--enable-auto-tool-choice"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "server_flag"
                && requirement["name"] == "--tool-call-parser"
                && requirement["value"] == "gemma4"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "chat_template"
                && requirement["value"] == "examples/tool_chat_template_gemma4.jinja"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "manifest_metadata"
                && requirement["name"] == "chat_template_hash"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    let warnings = report["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("Gemma 4 native tool parsing under vLLM"))),
        "warnings={warnings:?}"
    );
    assert_eq!(report["template"]["name"], "gemma4_native_function_calling");
    assert_eq!(
        report["template"]["source"],
        "Gemma 4 tokenizer/provider native function-calling chat template"
    );
    assert_eq!(
        report["template"]["supervised_target"],
        "assistant messages with native tool_calls plus paired tool role results"
    );
    let eval = report["evaluation"]["eval_command"]
        .as_array()
        .expect("eval argv");
    assert_eq!(report["evaluation"]["minimum_trials"], 5);
    assert_eq!(
        report["evaluation"]["comparison_baseline"],
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
    );
    let required_metrics = report["evaluation"]["required_metrics"]
        .as_array()
        .expect("required metrics");
    assert!(
        required_metrics.iter().any(|metric| metric
            .as_str()
            .is_some_and(|text| text.contains("native tool-call schema acceptance rate"))),
        "required metrics={required_metrics:?}"
    );
    let eval_gates = report["evaluation"]["gates"]
        .as_array()
        .expect("eval gates");
    assert!(
        eval_gates.iter().any(|gate| gate
            .as_str()
            .is_some_and(|text| text.contains("contract-id drift"))),
        "eval gates={eval_gates:?}"
    );
    let evidence = &report["evaluation"]["evidence_contract"];
    assert_eq!(
        evidence["schema_version"],
        LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION
    );
    let required_receipts = evidence["required_receipts"]
        .as_array()
        .expect("required receipts");
    assert!(
        required_receipts
            .iter()
            .any(|receipt| receipt == "promotion_probe_matrix_receipt"),
        "required receipts={required_receipts:?}"
    );
    let required_probe_cases = evidence["required_probe_cases"]
        .as_array()
        .expect("required probe cases");
    let probe_case_ids = required_probe_cases
        .iter()
        .filter_map(|probe_case| probe_case["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        probe_case_ids,
        vec![
            "sequential_tool_call",
            "parallel_tool_calls",
            "no_tool_answer",
            "unavailable_tool_repair",
            "multi_turn_tool_result_continuation",
            "serving_concurrency_probe",
        ]
    );
    assert!(
        required_probe_cases.iter().any(|probe_case| {
            probe_case["id"] == "parallel_tool_calls"
                && probe_case["requirement"]
                    .as_str()
                    .is_some_and(|requirement| {
                        requirement.contains("required_when_route_supports_parallel_tool_calls")
                    })
        }),
        "required probe cases={required_probe_cases:?}"
    );
    let probe_command_templates = evidence["probe_command_templates"]
        .as_array()
        .expect("probe command templates");
    assert_eq!(
        probe_command_templates.len(),
        required_probe_cases.len(),
        "probe command templates={probe_command_templates:?}"
    );
    let sequential_probe = probe_command_templates
        .iter()
        .find(|template| template["case_id"] == "sequential_tool_call")
        .expect("sequential probe command template");
    assert_eq!(
        sequential_probe["summary_path"],
        "PROMOTION_PROBES/sequential_tool_call/summary.json"
    );
    let sequential_command = sequential_probe["command"]
        .as_array()
        .expect("sequential probe argv");
    assert!(
        sequential_command
            .windows(2)
            .any(|pair| pair[0] == "--planner" && pair[1] == "provider=vllm,model=ADAPTER_MODEL"),
        "sequential probe argv={sequential_command:?}"
    );
    assert!(
        sequential_command
            .windows(2)
            .any(|pair| pair[0] == "--filter" && pair[1] == "sequential_tool_call"),
        "sequential probe argv={sequential_command:?}"
    );
    let concurrency_probe = probe_command_templates
        .iter()
        .find(|template| template["case_id"] == "serving_concurrency_probe")
        .expect("serving concurrency probe command template");
    assert!(
        concurrency_probe["notes"]
            .as_array()
            .expect("concurrency notes")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|text| text.contains("concurrent adapter-loaded requests"))),
        "serving concurrency probe={concurrency_probe:?}"
    );
    assert!(
        eval.windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "eval argv={eval:?}"
    );
    let launch = report["launch"]["local_launch_command"]
        .as_array()
        .expect("launch argv");
    assert!(
        launch.iter().any(|arg| arg == "--lora-adapter"),
        "launch argv={launch:?}"
    );
    assert!(
        launch
            .windows(2)
            .any(|pair| pair[0] == "--max-lora-rank" && pair[1] == "16"),
        "launch argv={launch:?}"
    );
}

#[test]
fn models_lora_plan_fixed_catalog_policy_is_explicit() {
    let missing_identity = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--tool-catalog-policy",
            "fixed-catalog-internalized",
            "--json",
        ],
        &[],
    );
    assert_ne!(missing_identity.exit_code, 0);
    assert!(
        missing_identity
            .stderr
            .contains("requires --tool-catalog-id or --tool-catalog-hash"),
        "stderr={}",
        missing_identity.stderr
    );

    let harn = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--tool-catalog-policy",
            "fixed-catalog-internalized",
            "--tool-catalog-id",
            "burin-tools-v1",
            "--tool-catalog-hash",
            "sha256:burin-tool-catalog",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    let catalog = &report["training"]["contract"]["tool_catalog"];
    assert_eq!(catalog["policy"], "fixed_catalog_internalized");
    assert_eq!(catalog["catalog_id"], "burin-tools-v1");
    assert_eq!(catalog["catalog_hash"], "sha256:burin-tool-catalog");
    assert_eq!(catalog["inference_catalog"], "no_runtime_catalog");
    assert_eq!(
        catalog["prompt_catalog_requirement"],
        "omit runtime tool catalog; adapter weights are bound to the declared fixed catalog"
    );
    let required_metadata = report["training"]["contract"]["required_example_metadata"]
        .as_array()
        .expect("required example metadata");
    for field in [
        "tool_catalog_policy",
        "tool_catalog_id",
        "tool_catalog_hash",
    ] {
        assert!(
            required_metadata.iter().any(|value| value == field),
            "required metadata missing {field}: {required_metadata:?}"
        );
    }
    assert_eq!(
        report["serving"]["tool_catalog"]["policy"],
        "fixed_catalog_internalized"
    );
    for command_name in ["export_command", "train_command", "manifest_command"] {
        let command = report["launch"][command_name]
            .as_array()
            .expect("command argv");
        assert!(
            command.windows(2).any(|pair| {
                pair[0] == "--tool-catalog-policy" && pair[1] == "fixed_catalog_internalized"
            }),
            "{command_name}={command:?}"
        );
        assert!(
            command
                .windows(2)
                .any(|pair| pair[0] == "--tool-catalog-id" && pair[1] == "burin-tools-v1"),
            "{command_name}={command:?}"
        );
        assert!(
            command.windows(2).any(|pair| {
                pair[0] == "--tool-catalog-hash" && pair[1] == "sha256:burin-tool-catalog"
            }),
            "{command_name}={command:?}"
        );
    }
}
