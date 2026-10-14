use std::io::Cursor;

use super::*;

#[test]
fn train_report_keeps_backend_launch_explicit_and_dry_run_by_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(
            &dataset,
            "{\"assistant_tool_text\":\"<tool_call>{\\\"name\\\":\\\"edit\\\",\\\"arguments\\\":{}}</tool_call><tool_call>{\\\"name\\\":\\\"run\\\",\\\"arguments\\\":{}}</tool_call>\",\"metadata\":{\"schema_repaired\":true}}\n",
        )
        .expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("local-vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: Some("burin-tools".to_string()),
        request_model: None,
        chat_template: None,
        trainer: "unsloth_trl_sft".to_string(),
        trainer_version: Some("unsloth-2026.7".to_string()),
        trainer_identity: None,
        observed_trainer_identity: Some("version=unsloth-2026.7".to_string()),
        method: "qlora".to_string(),
        rank: 24,
        alpha: None,
        dropout: 0.1,
        max_seq_length: Some(8192),
        teacher: None,
        target_metadata: vec!["lane=tool-calls".to_string()],
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: vec!["embed_tokens".to_string()],
        target_modules: Vec::new(),
        backend_recipe: "explicit_argv".to_string(),
        backend_runner: Vec::new(),
        backend_script: None,
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: vec![
            "uv".to_string(),
            "run".to_string(),
            "python".to_string(),
            "train.py".to_string(),
            "config.yaml".to_string(),
        ],
    };
    let report = train_report(&args).expect("report");
    assert_eq!(report.mode, "dry_run");
    assert_eq!(report.base.provider, "vllm");
    assert_eq!(report.serving.provider, "vllm");
    assert_eq!(report.training.trainer, "unsloth_sft");
    assert_eq!(
        report.training.trainer_version.as_deref(),
        Some("unsloth-2026.7")
    );
    assert_eq!(report.training.alpha, 48);
    assert_eq!(
        report.training.contract.peft_save_policy.modules_to_save,
        vec!["embed_tokens".to_string()]
    );
    assert!(
        report
            .training
            .contract
            .peft_save_policy
            .requires_weight_tying_check
    );
    assert_eq!(report.backend.recipe, "explicit_argv");
    assert_eq!(report.backend.argv_source, "explicit");
    assert_eq!(report.backend.status, "dry_run");
    assert!(!report.backend.execute);
    assert_eq!(report.backend.output_tail_bytes, BACKEND_OUTPUT_TAIL_BYTES);
    assert!(report.backend.stdout_tail.is_none());
    assert!(report.backend.stderr_tail.is_none());
    assert_eq!(report.inputs.dataset.kind, "file");
    assert_eq!(report.dataset_audit.rows, 1);
    assert_eq!(report.dataset_audit.parallel_tool_call_rows, 1);
    assert_eq!(report.dataset_audit.schema_repaired_rows, 1);
    assert_eq!(report.target.request_model, "burin-tools");
    assert!(report
        .post_training
        .manifest_command
        .windows(2)
        .any(|pair| pair == ["--provider", "vllm"]));
    assert!(report
        .post_training
        .manifest_command
        .windows(2)
        .any(|pair| pair == ["--trainer", "unsloth_sft"]));
    assert!(report
        .post_training
        .manifest_command
        .windows(2)
        .any(|pair| pair == ["--trainer-version", "unsloth-2026.7"]));
    assert!(report
        .post_training
        .manifest_command
        .windows(2)
        .any(|pair| pair == ["--modules-to-save", "embed_tokens"]));
    assert_eq!(
        report
            .target
            .metadata
            .get("serving_tool_parser_owner")
            .map(String::as_str),
        Some("harn_text_tool_parser")
    );
}

#[test]
fn train_report_without_backend_argv_records_backend_requirement() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: None,
        request_model: None,
        chat_template: None,
        trainer: "trl_sft_trainer".to_string(),
        trainer_version: None,
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: Vec::new(),
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "explicit_argv".to_string(),
        backend_runner: Vec::new(),
        backend_script: None,
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: Vec::new(),
    };
    let report = train_report(&args).expect("report");
    assert!(report.backend.argv.is_empty());
    assert!(report.backend.argv_required);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("no backend argv supplied")));
}

#[test]
fn train_report_renders_harn_lora_sft_recipe_backend_argv() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    let export_manifest = tmp.path().join("export.manifest.json");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    std::fs::write(&export_manifest, "{}\n").expect("manifest");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset: dataset.clone(),
        corpus: Some(tmp.path().join("corpus")),
        export_manifest: Some(export_manifest.clone()),
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: Some("burin-tools".to_string()),
        request_model: Some("burin-tools".to_string()),
        chat_template: Some("harn_text_tool_calls_json_fences".to_string()),
        trainer: "unsloth_sft".to_string(),
        trainer_version: Some("unsloth-2026.7".to_string()),
        trainer_identity: None,
        observed_trainer_identity: Some("version=unsloth-2026.7".to_string()),
        method: "lora".to_string(),
        rank: 32,
        alpha: Some(64),
        dropout: 0.1,
        max_seq_length: Some(8192),
        teacher: Some("dashscope/qwen3-coder-next".to_string()),
        target_metadata: vec!["lane=structured".to_string()],
        tool_catalog_policy: "fixed_catalog_internalized".to_string(),
        tool_catalog_id: Some("burin-tools-v1".to_string()),
        tool_catalog_hash: Some("sha256:burin-tool-catalog".to_string()),
        modules_to_save: vec!["embed_tokens".to_string(), "lm_head".to_string()],
        target_modules: vec!["q_proj".to_string(), "v_proj".to_string()],
        backend_recipe: "harn_lora_sft_v1".to_string(),
        backend_runner: vec!["uv".to_string(), "run".to_string(), "python".to_string()],
        backend_script: Some("train.py".into()),
        backend_config: Some("config/e4b.yaml".into()),
        backend_result_out: Some(tmp.path().join("backend-result.json")),
        execute: false,
        backend_cwd: Some(tmp.path().join("trainer")),
        json: true,
        backend_argv: Vec::new(),
    };
    let report = train_report(&args).expect("report");

    assert_eq!(report.backend.recipe, "harn_lora_sft_v1");
    assert_eq!(report.backend.argv_source, "recipe");
    let expected_cwd = tmp.path().join("trainer").display().to_string();
    assert_eq!(report.backend.cwd.as_deref(), Some(expected_cwd.as_str()));
    let first_four: Vec<&str> = report
        .backend
        .argv
        .iter()
        .take(4)
        .map(String::as_str)
        .collect();
    assert_eq!(first_four, vec!["uv", "run", "python", "train.py"]);
    let expected_pairs = vec![
        ("--dataset".to_string(), dataset.display().to_string()),
        (
            "--output-dir".to_string(),
            tmp.path().join("adapter").display().to_string(),
        ),
        ("--base".to_string(), "local-gemma4-e4b".to_string()),
        ("--provider".to_string(), "vllm".to_string()),
        ("--tool-format".to_string(), "json".to_string()),
        ("--adapter-name".to_string(), "burin-tools".to_string()),
        ("--request-model".to_string(), "burin-tools".to_string()),
        (
            "--chat-template".to_string(),
            "harn_text_tool_calls_json_fences".to_string(),
        ),
        ("--trainer".to_string(), "unsloth_sft".to_string()),
        (
            "--trainer-version".to_string(),
            "unsloth-2026.7".to_string(),
        ),
        ("--method".to_string(), "lora".to_string()),
        ("--rank".to_string(), "32".to_string()),
        ("--alpha".to_string(), "64".to_string()),
        ("--dropout".to_string(), "0.1".to_string()),
        ("--max-seq-length".to_string(), "8192".to_string()),
        (
            "--corpus".to_string(),
            tmp.path().join("corpus").display().to_string(),
        ),
        (
            "--export-manifest".to_string(),
            export_manifest.display().to_string(),
        ),
        (
            "--teacher".to_string(),
            "dashscope/qwen3-coder-next".to_string(),
        ),
        (
            "--tool-catalog-policy".to_string(),
            "fixed_catalog_internalized".to_string(),
        ),
        (
            "--tool-catalog-id".to_string(),
            "burin-tools-v1".to_string(),
        ),
        (
            "--tool-catalog-hash".to_string(),
            "sha256:burin-tool-catalog".to_string(),
        ),
        (
            "--target-metadata".to_string(),
            "lane=structured".to_string(),
        ),
        (
            "--backend-result-out".to_string(),
            tmp.path().join("backend-result.json").display().to_string(),
        ),
        ("--config".to_string(), "config/e4b.yaml".to_string()),
    ];
    for (flag, value) in expected_pairs {
        assert!(
            report
                .backend
                .argv
                .windows(2)
                .any(|pair| pair[0] == flag && pair[1] == value),
            "missing backend argv pair: {flag} {value}"
        );
    }
    assert_eq!(
        report
            .backend
            .argv
            .windows(2)
            .filter(|pair| *pair == ["--modules-to-save", "embed_tokens"])
            .count(),
        1
    );
    assert_eq!(
        report
            .backend
            .argv
            .windows(2)
            .filter(|pair| *pair == ["--modules-to-save", "lm_head"])
            .count(),
        1
    );
    assert_eq!(report.training.target_modules.policy, "explicit");
    let expected_result_path = tmp.path().join("backend-result.json").display().to_string();
    assert_eq!(
        report.backend.result_path.as_deref(),
        Some(expected_result_path.as_str())
    );
    assert_eq!(
        report.training.target_modules.modules,
        vec!["q_proj".to_string(), "v_proj".to_string()]
    );
    for module in ["q_proj", "v_proj"] {
        assert_eq!(
            report
                .backend
                .argv
                .windows(2)
                .filter(|pair| *pair == ["--target-modules", module])
                .count(),
            1
        );
        assert_eq!(
            report
                .post_training
                .manifest_command
                .windows(2)
                .filter(|pair| *pair == ["--target-modules", module])
                .count(),
            1
        );
    }
}

#[test]
fn backend_result_merges_runtime_metadata_into_harn_manifest_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: Some("burin-tools".to_string()),
        request_model: None,
        chat_template: None,
        trainer: "unsloth_sft".to_string(),
        trainer_version: Some("unsloth-2026.7".to_string()),
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: vec!["lane=structured".to_string()],
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "explicit_argv".to_string(),
        backend_runner: Vec::new(),
        backend_script: None,
        backend_config: None,
        backend_result_out: Some(tmp.path().join("backend.result.json")),
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: Vec::new(),
    };
    let mut report = train_report(&args).expect("report");
    let result = BackendResult {
        schema_version: 1,
        rendered_records: Some(197),
        trainable_records: Some(190),
        retention_ratio: Some(0.964),
        trainer_identity: Some(TrainerIdentity {
            schema_version: 1,
            kind: "version".to_string(),
            value: "unsloth-2026.7".to_string(),
        }),
        trainer_environment_observation: None,
        runtime: std::collections::BTreeMap::from([(
            "torch_version".to_string(),
            serde_json::Value::String("2.9.0".to_string()),
        )]),
        tokenizer: std::collections::BTreeMap::from([(
            "class".to_string(),
            serde_json::Value::String("GemmaTokenizerFast".to_string()),
        )]),
        artifacts: std::collections::BTreeMap::from([(
            "adapter_dir".to_string(),
            tmp.path().join("adapter").display().to_string(),
        )]),
        target_metadata: std::collections::BTreeMap::from([
            ("lane".to_string(), "structured".to_string()),
            ("rendered_records".to_string(), "197".to_string()),
            ("trainable_records".to_string(), "190".to_string()),
            ("retention_ratio".to_string(), "0.964".to_string()),
        ]),
        warnings: vec!["tokenizer added a pad token".to_string()],
    };

    apply_backend_result(&mut report, result).expect("backend result");

    assert_eq!(
        report
            .target
            .metadata
            .get("trainable_records")
            .map(String::as_str),
        Some("190")
    );
    assert!(report
        .post_training
        .manifest_command
        .windows(2)
        .any(|pair| pair == ["--target-metadata", "retention_ratio=0.964"]));
    assert_eq!(
        report
            .backend
            .result
            .as_ref()
            .and_then(|result| result.trainable_records),
        Some(190)
    );
    assert_eq!(report.training.trainer_identity.status, "matched");
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning == "backend result: tokenizer added a pad token"));
}

#[test]
fn backend_result_rejects_conflicting_harn_planned_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: None,
        request_model: None,
        chat_template: None,
        trainer: "unsloth_sft".to_string(),
        trainer_version: Some("unsloth-2026.7".to_string()),
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: vec!["lane=structured".to_string()],
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "explicit_argv".to_string(),
        backend_runner: Vec::new(),
        backend_script: None,
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: Vec::new(),
    };
    let mut report = train_report(&args).expect("report");
    let result = BackendResult {
        schema_version: 1,
        rendered_records: None,
        trainable_records: None,
        retention_ratio: None,
        trainer_identity: None,
        trainer_environment_observation: None,
        runtime: std::collections::BTreeMap::new(),
        tokenizer: std::collections::BTreeMap::new(),
        artifacts: std::collections::BTreeMap::new(),
        target_metadata: std::collections::BTreeMap::from([(
            "lane".to_string(),
            "backend-overrode-harn".to_string(),
        )]),
        warnings: Vec::new(),
    };

    let error = apply_backend_result(&mut report, result).expect_err("metadata conflict");

    assert!(error.contains("target metadata conflict"));
    assert!(!report.ok);
    assert_eq!(report.backend.status, "completed_metadata_conflict");
}

#[test]
fn train_report_rejects_recipe_options_in_explicit_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: None,
        request_model: None,
        chat_template: None,
        trainer: "external_sft_trainer".to_string(),
        trainer_version: None,
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: Vec::new(),
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "explicit_argv".to_string(),
        backend_runner: vec!["uv".to_string()],
        backend_script: None,
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: Vec::new(),
    };

    let error = train_report(&args).expect_err("recipe option should fail explicit mode");
    assert!(error.contains("require --backend-recipe harn_lora_sft_v1"));
}

#[test]
fn train_report_rejects_recipe_without_script() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: None,
        request_model: None,
        chat_template: None,
        trainer: "external_sft_trainer".to_string(),
        trainer_version: None,
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: Vec::new(),
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "harn-lora-sft-v1".to_string(),
        backend_runner: Vec::new(),
        backend_script: None,
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: Vec::new(),
    };

    let error = train_report(&args).expect_err("recipe without script should fail");
    assert!(error.contains("requires --backend-script"));
}

#[test]
fn train_report_rejects_raw_backend_argv_in_recipe_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("dataset.jsonl");
    std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let args = ModelsLoraTrainArgs {
        base_model: "local-gemma4-e4b".to_string(),
        provider: Some("vllm".to_string()),
        tool_format: "json".to_string(),
        dataset,
        corpus: None,
        export_manifest: None,
        output_dir: tmp.path().join("adapter"),
        receipt_out: None,
        adapter_name: None,
        request_model: None,
        chat_template: None,
        trainer: "external_sft_trainer".to_string(),
        trainer_version: None,
        trainer_identity: None,
        observed_trainer_identity: None,
        method: "qlora".to_string(),
        rank: 16,
        alpha: None,
        dropout: 0.05,
        max_seq_length: None,
        teacher: None,
        target_metadata: Vec::new(),
        tool_catalog_policy: "full_schema".to_string(),
        tool_catalog_id: None,
        tool_catalog_hash: None,
        modules_to_save: Vec::new(),
        target_modules: Vec::new(),
        backend_recipe: "harn_lora_sft_v1".to_string(),
        backend_runner: Vec::new(),
        backend_script: Some("train.py".into()),
        backend_config: None,
        backend_result_out: None,
        execute: false,
        backend_cwd: None,
        json: true,
        backend_argv: vec!["python".to_string(), "legacy_train.py".to_string()],
    };

    let error = train_report(&args).expect_err("recipe with raw argv should fail");
    assert!(error.contains("cannot be combined"));
}

#[test]
fn output_tail_keeps_bounded_suffix() {
    let mut tail = OutputTail::new(5);
    tail.push(b"abc");
    assert_eq!(tail.text().as_deref(), Some("abc"));
    assert!(!tail.truncated);
    tail.push(b"def");
    assert_eq!(tail.text().as_deref(), Some("bcdef"));
    assert!(tail.truncated);
    tail.push(b"ghijkl");
    assert_eq!(tail.text().as_deref(), Some("hijkl"));
    assert!(tail.truncated);
}

#[test]
fn backend_stream_capture_keeps_bounded_suffix_for_unbroken_output() {
    let tail = Arc::new(Mutex::new(OutputTail::new(5)));
    let input = Cursor::new(b"abcdefghijkl".to_vec());
    let mut mirrored = Vec::new();

    capture_backend_stream(input, &mut mirrored, Arc::clone(&tail), "stdout").unwrap();

    assert_eq!(mirrored, b"abcdefghijkl");
    let captured = output_tail(tail, "stdout");
    assert_eq!(captured.text.as_deref(), Some("hijkl"));
    assert!(captured.truncated);
}
