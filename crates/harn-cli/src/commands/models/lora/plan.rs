use super::*;

pub(super) fn plan_report(args: &ModelsLoraPlanArgs) -> Result<LoraPlanReport, String> {
    let method = normalize_lora_method(&args.method)?;
    let trainer = normalize_lora_trainer(&args.trainer)?;
    let expected_trainer_identity = trainer_identity_from_args(
        args.trainer_identity.as_deref(),
        args.trainer_version.as_deref(),
    )?;
    let trainer_identity = trainer_identity_check(expected_trainer_identity.clone(), None);
    let rank = normalize_lora_rank(args.rank)?;
    let alpha = normalize_lora_alpha(args.alpha, rank)?;
    let dropout = normalize_lora_dropout(args.dropout)?;
    let quantization = quantization_for_method(&method).to_string();
    let precision = precision_contract_for_method(&method);
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let requested_corpus_strategy = normalize_corpus_strategy(&args.corpus_strategy)?;
    let tool_catalog_policy = normalize_tool_catalog_policy(&args.tool_catalog_policy)?;
    let tool_catalog = tool_catalog_contract(
        &tool_catalog_policy,
        args.tool_catalog_id.as_deref(),
        args.tool_catalog_hash.as_deref(),
    )?;
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let modules_to_save = normalize_modules_to_save(&args.modules_to_save)?;
    let target_modules = target_module_contract(
        &args.target_modules,
        &method,
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
    )?;
    let provider = resolve_lora_provider(args.provider.as_deref(), &resolved.provider);
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &resolved.id);
    let catalog_default_tool_format =
        harn_vm::llm_config::default_tool_format(&resolved.id, &provider);
    let decision = if requested_tool_format == "auto" {
        harn_vm::llm::capabilities::ToolFormatDecision {
            effective: catalog_default_tool_format.clone(),
            correction: None,
        }
    } else {
        harn_vm::llm::capabilities::validate_tool_format(
            &provider,
            &resolved.id,
            &requested_tool_format,
        )
    };
    let dataset_format = dataset_format_for_tool_format(&decision.effective);
    let request_model = "ADAPTER_MODEL".to_string();
    let adapter_name = "ADAPTER_NAME".to_string();
    let adapter_ref = "ADAPTER_PATH_OR_REPO".to_string();
    let corpus = args
        .corpus
        .as_ref()
        .map(|corpus| corpus.trim().to_string())
        .filter(|corpus| !corpus.is_empty());
    let teacher = args
        .teacher
        .as_ref()
        .map(|selector| teacher_report(selector));
    let effective_corpus_strategy = effective_corpus_strategy(
        &requested_corpus_strategy,
        corpus.as_deref(),
        teacher.as_ref(),
    );
    let dataset_arg = corpus
        .clone()
        .unwrap_or_else(|| "conformance/tool-call-eval".to_string());
    let template = template_recipe_for_route(
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
        &decision.effective,
    );
    let contract_id = lora_contract_id(
        &resolved.id,
        &provider,
        &decision.effective,
        dataset_format,
        Some(&template.name),
        &target_modules,
        &modules_to_save,
        &tool_catalog,
    )?;
    let inspect_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "inspect".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--name".to_string(),
        adapter_name.clone(),
        adapter_ref.clone(),
    ];
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let serving = serving_recipe(ServingRecipeInput {
        base_model: &resolved.id,
        provider: &provider,
        request_model: &request_model,
        adapter_name: &adapter_name,
        tool_format: &decision.effective,
        dataset_format,
        provider_supports_lora_launch,
        lora_module_value_format: &lora_module_value_format,
        tool_catalog: &tool_catalog,
    });
    let launch_command = if provider_supports_lora_launch {
        let mut command = vec![
            "harn".to_string(),
            "local".to_string(),
            "launch".to_string(),
            args.base_model.clone(),
            "--provider".to_string(),
            provider.clone(),
            "--model-source".to_string(),
            resolved.id.clone(),
            "--lora-adapter".to_string(),
            format!("{adapter_name}={adapter_ref}"),
        ];
        if local_runtime
            .as_ref()
            .and_then(|runtime| runtime.max_lora_rank_arg.as_ref())
            .is_some()
        {
            command.extend(["--max-lora-rank".to_string(), rank.to_string()]);
        }
        command
    } else {
        Vec::new()
    };
    let eval_dataset = dataset_arg.clone();
    let eval_command = vec![
        "harn".to_string(),
        "eval".to_string(),
        "tool-calls".to_string(),
        "--planner".to_string(),
        request_model.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        dataset_arg,
    ];
    let export_corpus_arg = corpus
        .clone()
        .unwrap_or_else(|| "CORPUS_JSONL_OR_DIR".to_string());
    let preflight_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "preflight".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--corpus".to_string(),
        export_corpus_arg.clone(),
        "--source-tool-format".to_string(),
        source_tool_format_required_for_target(&decision.effective).to_string(),
        "--check".to_string(),
    ];
    let mut export_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "export".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--corpus".to_string(),
        export_corpus_arg.clone(),
        "--out".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
    ];
    export_command.extend(precision_target_metadata(&precision));
    export_command.extend(modules_to_save_args(&modules_to_save));
    export_command.extend(target_modules_args(&target_modules));
    export_command.extend(tool_catalog_args(&tool_catalog));
    let mut train_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "train".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--export-manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--output-dir".to_string(),
        "ADAPTER_OUTPUT_DIR".to_string(),
        "--receipt-out".to_string(),
        "ADAPTER_OUTPUT_DIR/train.receipt.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name.clone(),
        "--request-model".to_string(),
        request_model.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
        "--trainer".to_string(),
        trainer.clone(),
        "--method".to_string(),
        method.clone(),
        "--rank".to_string(),
        rank.to_string(),
        "--alpha".to_string(),
        alpha.to_string(),
        "--dropout".to_string(),
        dropout.to_string(),
    ];
    if let Some(trainer_version) = &args.trainer_version {
        train_command.extend(["--trainer-version".to_string(), trainer_version.clone()]);
    }
    train_command.extend(trainer_identity_args(expected_trainer_identity.as_ref()));
    if let Some(teacher) = &teacher {
        train_command.extend(["--teacher".to_string(), teacher.selector.clone()]);
    }
    train_command.extend(precision_target_metadata(&precision));
    train_command.extend(modules_to_save_args(&modules_to_save));
    train_command.extend(target_modules_args(&target_modules));
    train_command.extend(tool_catalog_args(&tool_catalog));
    train_command.extend(target_metadata_args_from_map(&serving_target_metadata(
        &serving,
    )));
    let mut manifest_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "manifest".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--corpus".to_string(),
        export_corpus_arg,
        "--export-manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--out".to_string(),
        "ADAPTER_OUTPUT_DIR/adapter.manifest.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name,
        "--adapter-path".to_string(),
        adapter_ref,
        "--request-model".to_string(),
        request_model.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
        "--trainer".to_string(),
        trainer.clone(),
        "--method".to_string(),
        method.clone(),
        "--rank".to_string(),
        rank.to_string(),
        "--alpha".to_string(),
        alpha.to_string(),
        "--dropout".to_string(),
        dropout.to_string(),
    ];
    if let Some(trainer_version) = &args.trainer_version {
        manifest_command.extend(["--trainer-version".to_string(), trainer_version.clone()]);
    }
    manifest_command.extend(trainer_identity_args(expected_trainer_identity.as_ref()));
    if let Some(teacher) = &teacher {
        manifest_command.extend(["--teacher".to_string(), teacher.selector.clone()]);
    }
    manifest_command.extend(precision_target_metadata(&precision));
    manifest_command.extend(modules_to_save_args(&modules_to_save));
    manifest_command.extend(target_modules_args(&target_modules));
    manifest_command.extend(tool_catalog_args(&tool_catalog));
    manifest_command.extend(target_metadata_args_from_map(&serving_target_metadata(
        &serving,
    )));
    let tool_probe_command = vec![
        "harn".to_string(),
        "provider".to_string(),
        "tool-probe".to_string(),
        provider.clone(),
        "--model".to_string(),
        request_model.clone(),
        "--mode".to_string(),
        "both".to_string(),
        "--repeat".to_string(),
        "5".to_string(),
        "--json".to_string(),
    ];
    let promote_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "promote".to_string(),
        "--train-receipt".to_string(),
        "ADAPTER_OUTPUT_DIR/train.receipt.json".to_string(),
        "--probe-root".to_string(),
        "PROMOTION_PROBES".to_string(),
        "--base-probe-root".to_string(),
        "BASE_PROMOTION_PROBES".to_string(),
        "--out".to_string(),
        "ADAPTER_OUTPUT_DIR/promotion.receipt.json".to_string(),
        "--check".to_string(),
    ];
    let mut warnings = plan_warnings(
        &provider,
        &decision,
        provider_supports_lora_launch,
        capabilities.native_tools,
        &requested_tool_format,
        &requested_corpus_strategy,
        &effective_corpus_strategy,
        teacher.as_ref(),
    );
    if decision.effective == "native"
        && provider == "vllm"
        && is_gemma4_route(&resolved.id, &resolved.family, &resolved.lineage)
    {
        warnings.push(
            "Gemma 4 native tool parsing under vLLM is part of the serving contract; serialize validation/eval traffic or pin a parser version proven concurrency-safe"
                .to_string(),
        );
    }
    if !trainer_identity.promotable {
        warnings.push("trainer identity is missing; dry-run plans are not promotable until train/manifest record matching expected and observed identity".to_string());
    }
    Ok(LoraPlanReport {
        ok: true,
        base: BaseModelReport {
            selector: args.base_model.clone(),
            id: resolved.id.clone(),
            provider: provider.clone(),
            resolved_alias: resolved.alias,
            tool_format: catalog_default_tool_format,
            tier: resolved.tier,
            family: resolved.family,
            lineage: resolved.lineage,
            catalog_name: catalog.as_ref().map(|model| model.name.clone()),
            context_window: catalog.as_ref().map(|model| model.context_window),
        },
        request: PlanRequest {
            method,
            requested_tool_format,
            effective_tool_format: decision.effective.clone(),
            tool_format_correction: decision.correction,
            corpus,
            requested_corpus_strategy,
            effective_corpus_strategy: effective_corpus_strategy.clone(),
            teacher: teacher.clone(),
            tool_catalog_policy: tool_catalog.policy.clone(),
            tool_catalog_id: tool_catalog.catalog_id.clone(),
            tool_catalog_hash: tool_catalog.catalog_hash.clone(),
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        training: TrainingRecipe {
            adapter_type: "peft_lora".to_string(),
            trainer: trainer.clone(),
            trainer_version: args.trainer_version.clone(),
            trainer_identity: trainer_identity.clone(),
            rank,
            alpha,
            dropout,
            quantization,
            loss_scope: "assistant_tool_calls".to_string(),
            packing: "off_by_default_for_tool_boundaries".to_string(),
            target_modules,
            contract: lora_training_contract(
                dataset_format,
                &decision.effective,
                &modules_to_save,
                &tool_catalog,
            ),
            trainer_contract: trainer_contract_for_dataset(
                dataset_format,
                &decision.effective,
                &trainer,
                &modules_to_save,
                &tool_catalog,
            ),
            notes: training_notes(&decision.effective),
        },
        precision,
        template,
        data: DataRecipe {
            dataset_format: dataset_format.to_string(),
            required_columns: required_columns_for_dataset(dataset_format),
            validation: validation_steps_for_dataset(dataset_format),
        },
        corpus_refresh: corpus_refresh_recipe(
            &effective_corpus_strategy,
            teacher.as_ref(),
            &decision.effective,
            dataset_format,
        ),
        evaluation: lora_evaluation_recipe(LoraEvaluationRecipeInput {
            contract_id: &contract_id,
            base_model: &resolved.id,
            provider: &provider,
            request_model: &request_model,
            tool_format: &decision.effective,
            eval_dataset: &eval_dataset,
            trainer_identity: Some(&trainer_identity),
            trainer_environment: None,
            eval_command,
        }),
        serving,
        launch: PlanLaunchHints {
            preflight_command,
            export_command,
            train_command,
            manifest_command,
            inspect_command,
            local_launch_command: launch_command,
            tool_probe_command,
            promote_command,
            request_model,
        },
        warnings,
    })
}
