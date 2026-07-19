use super::*;

pub(super) fn dataset_format_for_tool_format(tool_format: &str) -> &'static str {
    match tool_format {
        "native" => "messages_with_tool_calls",
        "json" => "harn_text_tool_calls_json_fences",
        "text" => "harn_text_tool_calls_heredoc",
        _ => "harn_text_tool_calls",
    }
}

pub(super) fn required_columns_for_dataset(dataset_format: &str) -> Vec<String> {
    match dataset_format {
        "messages_with_tool_calls" => vec!["messages".to_string(), "tools".to_string()],
        _ => vec![
            "messages".to_string(),
            "tools".to_string(),
            "assistant_tool_text".to_string(),
        ],
    }
}

pub(super) fn validation_steps_for_dataset(dataset_format: &str) -> Vec<String> {
    match dataset_format {
        "messages_with_tool_calls" => vec![
            "validate every assistant message has structured tool_calls or plain text, never both"
                .to_string(),
            "validate every tool role message is paired with an assistant tool call".to_string(),
            "validate every example carries the exact tool schemas exposed at inference"
                .to_string(),
        ],
        _ => vec![
            "parse assistant_tool_text with Harn's text tool-call parser".to_string(),
            "validate tool names and arguments against the inference tool schemas".to_string(),
            "reject prose around tool calls unless the target parser explicitly accepts it"
                .to_string(),
        ],
    }
}

pub(super) fn source_tool_format_required_for_target(tool_format: &str) -> &'static str {
    match tool_format {
        "text" => "text",
        "json" | "native" => "json",
        _ => "auto",
    }
}

pub(super) fn training_notes(tool_format: &str) -> Vec<String> {
    match tool_format {
        "native" => vec![
            "train chat examples in the model's native tools/messages shape".to_string(),
            "preserve a tools/schema column so inference and training share one contract"
                .to_string(),
        ],
        "json" => vec![
            "train assistant completions to emit Harn fenced-JSON text tool calls".to_string(),
            "keep assistant-only loss so prompts and tool results are not learned as targets"
                .to_string(),
        ],
        "text" => vec![
            "train assistant completions to emit Harn heredoc-capable text tool calls".to_string(),
            "keep assistant-only loss so prompts and tool results are not learned as targets"
                .to_string(),
        ],
        _ => vec!["train against the route's validated tool-call format".to_string()],
    }
}

pub(super) fn trainer_contract_for_dataset(
    dataset_format: &str,
    tool_format: &str,
    trainer: &str,
    modules_to_save: &[String],
    tool_catalog: &ToolCatalogContract,
) -> Vec<String> {
    let machine_contract =
        lora_training_contract(dataset_format, tool_format, modules_to_save, tool_catalog);
    let peft_policy = &machine_contract.peft_save_policy;
    let mut contract = vec![
        "use TRL SFTTrainer with PEFT LoRA/QLoRA; keep the base weights frozen and save only adapter artifacts".to_string(),
        "set assistant_only_loss=true so prompts, tool schemas, and tool observations are context rather than targets".to_string(),
        "verify the tokenizer chat template emits assistant generation masks before trusting assistant_only_loss".to_string(),
        "keep packing=false unless a boundary-aware packer preserves complete tool-call/tool-result pairs".to_string(),
        format!(
            "inference tool catalog policy={}; catalog prompt requirement={}",
            machine_contract.tool_catalog.policy,
            machine_contract.tool_catalog.prompt_catalog_requirement
        ),
        format!(
            "set PEFT modules_to_save={}; keep embedding/lm_head saves explicit in the manifest",
            if peft_policy.modules_to_save.is_empty() {
                "[]".to_string()
            } else {
                format!("{:?}", peft_policy.modules_to_save)
            }
        ),
        "if embed_tokens or lm_head are saved for a tied-output base, verify PEFT weight tying before merge or keep the adapter unmerged".to_string(),
    ];
    match dataset_format {
        "messages_with_tool_calls" => {
            contract.push(
                "each record must include messages plus a tools column; assistant tool_calls and tool role messages stay paired".to_string(),
            );
        }
        _ => {
            contract.push(
                "each record must include messages, tools, and assistant_tool_text; parse assistant_tool_text with Harn before tokenization".to_string(),
            );
        }
    }
    if matches!(tool_format, "text" | "json") {
        contract.push(
            "do not train provider-native tool tags for Harn text/json routes; Harn remains the parser at inference".to_string(),
        );
    }
    match trainer {
        "unsloth_sft" => {
            contract.push(
                "use Unsloth only as the trainer backend; Harn remains the authority for export, manifest, eval, and serving contracts".to_string(),
            );
            contract.push(
                "enable Unsloth gradient checkpointing for long tool-call transcripts and keep packing disabled unless the packer preserves tool boundaries".to_string(),
            );
            contract.push(
                "record torch/CUDA, tokenizer class, and chat-template hash in `harn models lora manifest --target-metadata` after training".to_string(),
            );
        }
        "external_sft_trainer" => {
            contract.push(
                "external trainers must reproduce the Harn trainer contract exactly and stamp their backend/version in the LoRA manifest".to_string(),
            );
        }
        "mlx_lm" => {
            contract.push(
                "use mlx-lm only as the trainer backend; Harn remains the authority for export, manifest, eval, and serving contracts".to_string(),
            );
            contract.push(
                "record mlx-lm, MLX, macOS, and Apple Silicon hardware versions in `harn models lora manifest --target-metadata` after training".to_string(),
            );
            contract.push(
                "verify the produced adapter format can be served by the selected local runtime before promotion evidence is accepted".to_string(),
            );
        }
        _ => {
            contract.push(
                "use the stock TRL/PEFT backend unless a named trainer backend is recorded in the manifest".to_string(),
            );
        }
    }
    contract.push(format!(
        "machine contract: mask={} packing={} parser_owner={} split={}",
        machine_contract.assistant_mask_policy,
        machine_contract.packing_policy,
        machine_contract.tool_parser_owner,
        machine_contract.dataset_split_policy
    ));
    contract
}

pub(super) fn lora_training_contract(
    dataset_format: &str,
    tool_format: &str,
    modules_to_save: &[String],
    tool_catalog: &ToolCatalogContract,
) -> LoraTrainingContract {
    let mut required_example_metadata = vec![
        "dataset_format".to_string(),
        "source_tool_format".to_string(),
        "source_record_id".to_string(),
        "source_transcript_id".to_string(),
        "teacher_model".to_string(),
        "teacher_provider".to_string(),
        "target_base_model".to_string(),
        "target_tool_format".to_string(),
        "tool_schema_hash".to_string(),
        "prompt_template_hash".to_string(),
        "split".to_string(),
        "license".to_string(),
        "lora_contract_id".to_string(),
        "lora_target".to_string(),
        "tool_catalog_policy".to_string(),
    ];
    if tool_catalog.catalog_id.is_some() {
        required_example_metadata.push("tool_catalog_id".to_string());
    }
    if tool_catalog.catalog_hash.is_some() {
        required_example_metadata.push("tool_catalog_hash".to_string());
    }
    LoraTrainingContract {
        schema_version: LORA_TRAINING_CONTRACT_SCHEMA_VERSION,
        loss_scope: "assistant_tool_calls".to_string(),
        assistant_mask_policy: "require_chat_template_generation_masks".to_string(),
        packing_policy: "disabled_unless_boundary_aware_tool_pack_pairs".to_string(),
        tool_parser_owner: tool_parser_owner_for_format(tool_format).to_string(),
        dataset_format: dataset_format.to_string(),
        dataset_split_policy: "train_tune_holdout_disjoint_no_eval_holdout_training".to_string(),
        tool_catalog: tool_catalog.clone(),
        peft_save_policy: peft_save_policy(modules_to_save),
        required_example_metadata,
    }
}

pub(super) fn peft_save_policy(modules_to_save: &[String]) -> PeftSavePolicy {
    let saves_embeddings = modules_to_save
        .iter()
        .any(|module| matches!(module.as_str(), "embed_tokens" | "lm_head"));
    PeftSavePolicy {
        schema_version: LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION,
        modules_to_save: modules_to_save.to_vec(),
        save_embedding_layers: if saves_embeddings {
            "explicit_modules_to_save_declared".to_string()
        } else {
            "disabled_unless_tokenizer_vocab_changed".to_string()
        },
        tied_embedding_policy: if saves_embeddings {
            "verify_tied_embed_tokens_lm_head_remain_tied_before_merge_or_keep_adapter_unmerged"
                .to_string()
        } else {
            "no_embedding_or_lm_head_adapter_weights_expected".to_string()
        },
        requires_weight_tying_check: saves_embeddings,
        notes: vec![
            "default tool-calling LoRA/QLoRA adapters save only adapter weights".to_string(),
            "declare embed_tokens or lm_head only when tokenizer vocabulary or output-head training requires it".to_string(),
            "record tokenizer resize and weight-tying evidence in target metadata when saving embedding/head modules".to_string(),
        ],
    }
}

pub(super) fn lora_contract_id(
    base_model: &str,
    provider: &str,
    harn_tool_format: &str,
    dataset_format: &str,
    chat_template: Option<&str>,
    target_modules: &TargetModuleContract,
    modules_to_save: &[String],
    tool_catalog: &ToolCatalogContract,
) -> Result<String, String> {
    let input = LoraContractHashInput {
        schema_version: LORA_CONTRACT_HASH_SCHEMA_VERSION,
        base_model,
        provider,
        harn_tool_format,
        dataset_format,
        chat_template,
        target_module_policy: &target_modules.policy,
        target_modules: &target_modules.modules,
        modules_to_save,
        tool_catalog_policy: &tool_catalog.policy,
        tool_catalog_id: tool_catalog.catalog_id.as_deref(),
        tool_catalog_hash: tool_catalog.catalog_hash.as_deref(),
    };
    let bytes = serde_json::to_vec(&input)
        .map_err(|error| format!("failed to render LoRA contract hash input: {error}"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

pub(super) struct LoraContractReportInput<'a> {
    pub(super) contract_id: String,
    pub(super) base_model: &'a str,
    pub(super) provider: &'a str,
    pub(super) harn_tool_format: &'a str,
    pub(super) dataset_format: &'a str,
    pub(super) chat_template: Option<String>,
    pub(super) target_modules: &'a TargetModuleContract,
    pub(super) modules_to_save: &'a [String],
    pub(super) tool_catalog: &'a ToolCatalogContract,
}

pub(super) fn lora_contract_report(input: LoraContractReportInput<'_>) -> LoraContractReport {
    LoraContractReport {
        schema_version: LORA_CONTRACT_SCHEMA_VERSION,
        id: input.contract_id,
        base_model: input.base_model.to_string(),
        provider: input.provider.to_string(),
        harn_tool_format: input.harn_tool_format.to_string(),
        dataset_format: input.dataset_format.to_string(),
        chat_template: input.chat_template,
        target_modules: input.target_modules.clone(),
        training_contract: lora_training_contract(
            input.dataset_format,
            input.harn_tool_format,
            input.modules_to_save,
            input.tool_catalog,
        ),
    }
}

pub(super) fn tool_parser_owner_for_format(tool_format: &str) -> &'static str {
    match tool_format {
        "native" => "provider_tokenizer_runtime",
        "text" | "json" => "harn_text_tool_parser",
        _ => "catalog_validated_route",
    }
}

pub(super) fn lora_adapter_binding(provider_supports_lora_launch: bool) -> &'static str {
    if provider_supports_lora_launch {
        "runtime_lora_adapter"
    } else {
        "external_runtime_or_merged_adapter"
    }
}

pub(super) fn lora_modules_value_format(
    local_runtime: Option<&harn_vm::llm_config::LocalRuntimeDef>,
) -> String {
    local_runtime
        .and_then(|runtime| runtime.lora_modules_value_format.as_deref())
        .unwrap_or("name_path")
        .to_string()
}

pub(super) struct ServingRecipeInput<'a> {
    pub(super) base_model: &'a str,
    pub(super) provider: &'a str,
    pub(super) request_model: &'a str,
    pub(super) adapter_name: &'a str,
    pub(super) tool_format: &'a str,
    pub(super) dataset_format: &'a str,
    pub(super) provider_supports_lora_launch: bool,
    pub(super) lora_module_value_format: &'a str,
    pub(super) tool_catalog: &'a ToolCatalogContract,
}

pub(super) fn serving_recipe(input: ServingRecipeInput<'_>) -> ServingRecipe {
    let ServingRecipeInput {
        base_model,
        provider,
        request_model,
        adapter_name,
        tool_format,
        dataset_format,
        provider_supports_lora_launch,
        lora_module_value_format,
        tool_catalog,
    } = input;
    let adapter_binding = lora_adapter_binding(provider_supports_lora_launch).to_string();
    let mut runtime_notes = Vec::new();
    if provider_supports_lora_launch {
        runtime_notes.push(
            "serve the base model once and select the LoRA adapter per request model name"
                .to_string(),
        );
        runtime_notes.push(
            "keep adapter names stable across train, inspect, local launch, and eval reports"
                .to_string(),
        );
    } else {
        runtime_notes.push(
            "register the adapter in the external runtime or merge it only after promotion gates pass"
                .to_string(),
        );
        runtime_notes.push(
            "record the runtime-specific adapter binding in the export manifest metadata"
                .to_string(),
        );
    }
    runtime_notes.push(
        "do not change the tool-call format between dataset export, serving, and evaluation"
            .to_string(),
    );
    runtime_notes.extend(tool_call_serving_notes(base_model, provider, tool_format));
    let serving_requirements = tool_call_serving_requirements(base_model, provider, tool_format);
    ServingRecipe {
        request_model: request_model.to_string(),
        adapter_name: adapter_name.to_string(),
        base_model: base_model.to_string(),
        provider: provider.to_string(),
        adapter_binding,
        lora_module_value_format: lora_module_value_format.to_string(),
        tool_format: tool_format.to_string(),
        dataset_format: dataset_format.to_string(),
        tool_catalog: tool_catalog.clone(),
        serving_requirements,
        runtime_notes,
        promotion_gates: vec![
            "inspect the adapter against the exact served base model before launch".to_string(),
            "run base-versus-adapter tool-call evals with the same request model selector"
                .to_string(),
            "keep a rollback path to the base route or previous adapter revision".to_string(),
        ],
    }
}

pub(super) fn tool_call_serving_requirements(
    base_model: &str,
    provider: &str,
    tool_format: &str,
) -> Vec<ServingRequirement> {
    let mut requirements = Vec::new();
    if matches!(tool_format, "text" | "json") {
        requirements.push(serving_requirement(
            "parser_owner",
            "tool_call_parser",
            Some("harn_text_tool_parser"),
            true,
            "Harn parses text/json tool calls for this route.",
        ));
        requirements.push(serving_requirement(
            "provider_native_tool_parser",
            "native_tool_parser_mode",
            Some("disabled_unless_proxy_maps_to_harn_text"),
            true,
            "Provider-native tool parsers must not reinterpret Harn text tool calls.",
        ));
        return requirements;
    }

    if tool_format != "native" {
        requirements.push(serving_requirement(
            "parser_owner",
            "tool_call_parser",
            Some("catalog_validated_route"),
            true,
            "Resolve the effective route before serving.",
        ));
        return requirements;
    }

    requirements.push(serving_requirement(
        "parser_owner",
        "tool_call_parser",
        Some("provider_tokenizer_runtime"),
        true,
        "Native tool routes rely on the provider/tokenizer runtime parser.",
    ));

    if provider == "vllm" {
        requirements.push(serving_requirement(
            "server_flag",
            "--enable-auto-tool-choice",
            None,
            true,
            "vLLM native tool routes need automatic tool-call extraction enabled.",
        ));
        if is_functiongemma_route(base_model, "", "") {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("functiongemma"),
                true,
                "FunctionGemma native routes need the matching vLLM parser.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("functiongemma_control_tokens"),
                true,
                "Training and serving must share the FunctionGemma control-token template.",
            ));
            requirements.push(serving_requirement(
                "stop_sequence",
                "inference_stop_sequence",
                Some("<start_function_response>"),
                true,
                "Function response control tokens must not be generated as ordinary text.",
            ));
        } else if is_gemma4_route(base_model, "", "") {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("gemma4"),
                true,
                "Gemma 4 native routes need vLLM's Gemma 4 tool-call parser.",
            ));
            requirements.push(serving_requirement(
                "server_flag",
                "--reasoning-parser",
                Some("gemma4"),
                false,
                "Required when Gemma 4 thinking traces are enabled; keep explicit in launch manifests.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("examples/tool_chat_template_gemma4.jinja"),
                true,
                "Gemma 4 tool calling depends on the vLLM-compatible tool chat template.",
            ));
            requirements.push(serving_requirement(
                "manifest_metadata",
                "tool_parser_id",
                Some("gemma4"),
                true,
                "Persist the parser id so inspect can detect serving-route drift.",
            ));
            requirements.push(serving_requirement(
                "manifest_metadata",
                "chat_template_hash",
                None,
                true,
                "Persist the chat-template hash so training and serving can be compared.",
            ));
            requirements.push(serving_requirement(
                "promotion_gate",
                "parser_concurrency_policy",
                Some("serialize_validation_or_pin_parser_version"),
                true,
                "Promotion must prove the exact parser/template route used in serving.",
            ));
        } else {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("model_family_parser"),
                true,
                "vLLM native tool routes need the parser matching the model family.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("model_family_tool_template"),
                true,
                "Training and serving must share the model-family tool template.",
            ));
        }
    }

    requirements
}

pub(super) fn serving_requirement(
    kind: &str,
    name: &str,
    value: Option<&str>,
    required: bool,
    reason: &str,
) -> ServingRequirement {
    ServingRequirement {
        kind: kind.to_string(),
        name: name.to_string(),
        value: value.map(str::to_string),
        required,
        reason: reason.to_string(),
    }
}

pub(super) fn tool_call_serving_notes(
    base_model: &str,
    provider: &str,
    tool_format: &str,
) -> Vec<String> {
    let mut notes = Vec::new();
    if matches!(tool_format, "text" | "json") {
        notes.push(
            "serve the adapter as a text-channel route: Harn owns tool-call parsing for this plan"
                .to_string(),
        );
        notes.push(
            "keep provider-native tool parsers disabled unless the proxy maps them back to Harn text tool calls"
                .to_string(),
        );
        return notes;
    }
    if tool_format != "native" {
        return notes;
    }

    notes.push(
        "prefer schema-constrained or strict tool calling during serving and eval when the runtime supports it"
            .to_string(),
    );
    if provider == "vllm" {
        notes.push(
            "for vLLM native tools, serve with --enable-auto-tool-choice and the model family's matching --tool-call-parser/chat-template"
                .to_string(),
        );
    }
    if is_functiongemma_route(base_model, "", "") {
        notes.push(
            "FunctionGemma routes need the functiongemma parser/chat template and <start_function_response> stop handling"
                .to_string(),
        );
    } else if is_gemma4_route(base_model, "", "") {
        notes.push(
            "Gemma 4 native routes must keep the tokenizer/provider tool declaration, call, and response template identical between training and serving"
                .to_string(),
        );
        if provider == "vllm" {
            notes.push(
                "serialize Gemma 4 native-tool validation traffic or pin a vLLM release whose gemma4 parser is concurrency-safe before promotion"
                    .to_string(),
            );
            notes.push(
                "record the vLLM gemma4 tool-call parser and chat-template revision in the LoRA manifest"
                    .to_string(),
            );
        }
    }
    notes
}

pub(super) fn teacher_report(selector: &str) -> TeacherReport {
    let resolved = harn_vm::llm_config::resolve_model_info(selector);
    let provider = resolved.provider.clone();
    TeacherReport {
        selector: selector.to_string(),
        id: resolved.id.clone(),
        provider,
        resolved_alias: resolved.alias,
        tool_format: harn_vm::llm_config::default_tool_format(&resolved.id, &resolved.provider),
        family: resolved.family,
        lineage: resolved.lineage,
    }
}

pub(super) fn corpus_refresh_recipe(
    strategy: &str,
    teacher: Option<&TeacherReport>,
    tool_format: &str,
    dataset_format: &str,
) -> CorpusRefreshRecipe {
    let teacher_required = matches!(strategy, "refresh" | "distill");
    let mut generation_notes = match strategy {
        "refresh" => vec![
            "use the teacher to repair or extend existing corpus records; preserve stable ids for unchanged examples".to_string(),
            "write new examples only into train/tune splits until a separate holdout review promotes them".to_string(),
        ],
        "distill" => vec![
            "use the teacher to generate synthetic task/tool/result trajectories from frozen tool schemas".to_string(),
            "sample single-turn and multi-turn cases separately so turn-repair behavior remains measurable".to_string(),
        ],
        _ => vec![
            "audit the supplied corpus without synthetic generation before training".to_string(),
            "prefer parser/schema fixes over adding near-duplicate examples".to_string(),
        ],
    };
    generation_notes.push(format!(
        "render every accepted example in the effective `{tool_format}` tool-call convention"
    ));
    generation_notes.push(format!(
        "store examples in `{dataset_format}` form so training and eval consume one contract"
    ));
    if let Some(teacher) = teacher {
        generation_notes.push(format!(
            "record teacher route {} via {} for every synthetic or repaired record",
            teacher.id, teacher.provider
        ));
    }
    CorpusRefreshRecipe {
        strategy: strategy.to_string(),
        teacher_required,
        teacher: teacher.cloned(),
        generation_notes,
        provenance_manifest_fields: vec![
            "source_record_id".to_string(),
            "source_transcript_id".to_string(),
            "teacher_model".to_string(),
            "teacher_provider".to_string(),
            "target_base_model".to_string(),
            "target_tool_format".to_string(),
            "tool_schema_hash".to_string(),
            "prompt_template_hash".to_string(),
            "split".to_string(),
            "license".to_string(),
        ],
        hard_negative_slices: vec![
            "wrong-tool disambiguation under similar schemas".to_string(),
            "malformed-call repair without executing unsafe arguments".to_string(),
            "permission-denied or no-write tool outcomes".to_string(),
            "tool-result follow-up after partial or empty results".to_string(),
            "multi-turn correction after stale or contradictory observations".to_string(),
        ],
        acceptance_gates: vec![
            "target parser accepts every assistant tool-call target".to_string(),
            "tool names and arguments validate against the frozen inference schemas".to_string(),
            "dedupe by normalized tool name, arguments, and outcome class".to_string(),
            "train/tune/holdout splits stay disjoint from Harn and Burin eval holdouts".to_string(),
            "base-versus-adapter eval runs on identical cases before promotion".to_string(),
        ],
        model_aware_selection: model_aware_selection_recipe(strategy, tool_format, dataset_format),
    }
}

pub(super) fn model_aware_selection_recipe(
    strategy: &str,
    tool_format: &str,
    dataset_format: &str,
) -> ModelAwareSelectionRecipe {
    let parser_signal = if matches!(tool_format, "text" | "json") {
        "Harn text-parser failure class and repair distance"
    } else {
        "native tool-call schema validation error class"
    };
    let generation_scope = match strategy {
        "refresh" => "score existing and teacher-repaired records before adding them to train/tune",
        "distill" => {
            "score synthetic teacher trajectories before accepting them into a generated corpus"
        }
        _ => "score the supplied corpus and report gaps without generating new records",
    };
    ModelAwareSelectionRecipe {
        objective: "train on examples that expose the target base model's tool-use failure modes, not on near-duplicate syntax drills"
            .to_string(),
        difficulty_signals: vec![
            "target base-model outcome bucket: solved, missing_call, malformed_call, wrong_tool, bad_arguments, unsafe_or_unpermitted_call, premature_final_answer"
                .to_string(),
            parser_signal.to_string(),
            "tool schema overlap and argument-shape ambiguity for wrong-tool disambiguation"
                .to_string(),
            "turn-repair state: first-call, tool-result follow-up, permission denial, stale observation, or corrective retry"
                .to_string(),
            format!("dataset contract `{dataset_format}` sequence-fit and assistant-mask validity"),
        ],
        sampling_policy: vec![
            generation_scope.to_string(),
            "prioritize medium-difficulty candidates the base model nearly solves; cap already-solved and impossible examples"
                .to_string(),
            "balance single-turn calls, multi-turn repair, no-tool final answers, and permission/no-write outcomes"
                .to_string(),
            "dedupe by normalized tool name, arguments, outcome class, language, and task type before training"
                .to_string(),
        ],
        refinement_loop: vec![
            "round 0: preflight the frozen corpus, schemas, template, and split manifest"
                .to_string(),
            "round N: run the current base-or-adapter route on tune cases, bucket failures, then add or reweight only parser-valid teacher repairs"
                .to_string(),
            "keep holdout frozen before the first scoring pass; never recycle holdout failures into train/tune without changing the split id"
                .to_string(),
            "rerun base-versus-adapter eval on identical cases after each accepted refresh round"
                .to_string(),
        ],
        stop_conditions: vec![
            "no new failure bucket improves after a refresh round".to_string(),
            "paired tune lift is positive but holdout remains unmeasured; stop before promotion"
                .to_string(),
            "adapter regresses non-tool chat, safe refusal, or no-write cases".to_string(),
        ],
    }
}

pub(super) struct LoraEvaluationRecipeInput<'a> {
    pub(super) contract_id: &'a str,
    pub(super) base_model: &'a str,
    pub(super) provider: &'a str,
    pub(super) request_model: &'a str,
    pub(super) tool_format: &'a str,
    pub(super) eval_dataset: &'a str,
    pub(super) trainer_identity: Option<&'a TrainerIdentityCheck>,
    pub(super) trainer_environment: Option<&'a TrainerEnvironmentCheck>,
    pub(super) eval_command: Vec<String>,
}

pub(super) fn lora_evaluation_recipe(input: LoraEvaluationRecipeInput<'_>) -> EvaluationRecipe {
    let LoraEvaluationRecipeInput {
        contract_id,
        base_model,
        provider,
        request_model,
        tool_format,
        eval_dataset,
        trainer_identity,
        trainer_environment,
        eval_command,
    } = input;
    let parser_metric = if matches!(tool_format, "text" | "json") {
        "Harn text parser acceptance rate"
    } else {
        "native tool-call schema acceptance rate"
    };
    let minimum_trials = 5;
    let comparison_baseline =
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
            .to_string();
    let required_metrics = vec![
        "exact tool-name + argument match rate".to_string(),
        parser_metric.to_string(),
        "malformed-call and prose-only failure rate".to_string(),
        "wrong-tool false positive rate".to_string(),
        "latency and cost per solved tool-call case".to_string(),
    ];
    let gates = vec![
        "compare base versus adapter on identical tool-call cases".to_string(),
        "require a positive paired lift before promotion; inconclusive movement stays experimental"
            .to_string(),
        "require zero contract-id drift between export manifest, adapter metadata, and served route"
            .to_string(),
        "require matching expected and observed trainer identity before promotion".to_string(),
        "require Harn-normalized trainer environment attestation before promotion".to_string(),
        "require no regression on non-tool chat smoke prompts".to_string(),
    ];
    let evidence_contract = lora_promotion_evidence_contract(PromotionEvidenceInput {
        contract_id,
        base_model,
        provider,
        request_model,
        tool_format,
        eval_dataset,
        minimum_trials,
        required_metrics: &required_metrics,
        gates: &gates,
        trainer_identity,
        trainer_environment,
    });
    EvaluationRecipe {
        holdout_policy:
            "keep train/tune/holdout splits disjoint; never train on Harn eval fixtures".to_string(),
        minimum_trials,
        comparison_baseline,
        required_metrics,
        gates,
        evidence_contract,
        eval_command,
    }
}

/// Rebuild promotion evidence after execution finalizes provenance.
///
/// The initial train plan intentionally carries a non-promotable environment
/// placeholder. Backend execution can change both provenance checks, so the
/// promotion id must be derived again from the finalized Harn-owned values.
pub(super) fn refresh_lora_promotion_evidence(
    recipe: &mut EvaluationRecipe,
    trainer_identity: &TrainerIdentityCheck,
    trainer_environment: &TrainerEnvironmentCheck,
) {
    let evidence = &recipe.evidence_contract;
    let contract_id = evidence.lora_contract_id.clone();
    let base_model = evidence.base_route.model.clone();
    let provider = evidence.base_route.provider.clone();
    let request_model = evidence.adapter_route.model.clone();
    let tool_format = evidence.adapter_route.tool_format.clone();
    let eval_dataset = evidence.eval_dataset.clone();
    let minimum_trials = evidence.minimum_trials;
    recipe.evidence_contract = lora_promotion_evidence_contract(PromotionEvidenceInput {
        contract_id: &contract_id,
        base_model: &base_model,
        provider: &provider,
        request_model: &request_model,
        tool_format: &tool_format,
        eval_dataset: &eval_dataset,
        minimum_trials,
        required_metrics: &recipe.required_metrics,
        gates: &recipe.gates,
        trainer_identity: Some(trainer_identity),
        trainer_environment: Some(trainer_environment),
    });
}

pub(super) fn lora_promotion_evidence_contract(
    input: PromotionEvidenceInput<'_>,
) -> PromotionEvidenceContract {
    let required_probe_cases = lora_required_probe_cases(input.tool_format);
    let probe_command_templates =
        lora_promotion_probe_command_templates(&input, &required_probe_cases);
    let promotion_id = lora_promotion_id(&input, &required_probe_cases, &probe_command_templates);
    PromotionEvidenceContract {
        schema_version: LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION,
        promotion_id,
        lora_contract_id: input.contract_id.to_string(),
        base_route: PromotionRoute {
            role: "base".to_string(),
            provider: input.provider.to_string(),
            model: input.base_model.to_string(),
            tool_format: input.tool_format.to_string(),
        },
        adapter_route: PromotionRoute {
            role: "adapter".to_string(),
            provider: input.provider.to_string(),
            model: input.request_model.to_string(),
            tool_format: input.tool_format.to_string(),
        },
        trainer_identity: input.trainer_identity.cloned(),
        trainer_environment: input.trainer_environment.cloned(),
        eval_dataset: input.eval_dataset.to_string(),
        minimum_trials: input.minimum_trials,
        required_receipts: vec![
            "lora_preflight_report".to_string(),
            "lora_export_manifest".to_string(),
            "lora_adapter_manifest".to_string(),
            "lora_train_receipt".to_string(),
            "lora_inspect_report".to_string(),
            "tool_probe_receipt".to_string(),
            "promotion_probe_matrix_receipt".to_string(),
            "base_eval_receipt".to_string(),
            "adapter_eval_receipt".to_string(),
        ],
        required_probe_cases,
        probe_command_templates,
        optional_batch_receipts: vec![
            "harn.model_batch_manifest".to_string(),
            "harn.model_batch_prepare_receipt".to_string(),
            "harn.model_batch_submission_receipt".to_string(),
            "harn.model_batch_status_receipt".to_string(),
            "harn.model_batch_results_receipt".to_string(),
        ],
        batch_ready: PromotionBatchReady {
            workload: "eval".to_string(),
            group_by: vec![
                "provider".to_string(),
                "model".to_string(),
                "tool_format".to_string(),
                "lora_contract_id".to_string(),
                "promotion_id".to_string(),
            ],
            request_row_contract: vec![
                "custom_id".to_string(),
                "provider".to_string(),
                "model".to_string(),
                "tool_format".to_string(),
                "messages".to_string(),
                "tools".to_string(),
                "metadata.promotion_id".to_string(),
                "metadata.lora_contract_id".to_string(),
                "metadata.route_role".to_string(),
                "metadata.case_id".to_string(),
            ],
            manifest_command: vec![
                "harn".to_string(),
                "models".to_string(),
                "batch".to_string(),
                "manifest".to_string(),
                "--workload".to_string(),
                "eval".to_string(),
                "--tool-format".to_string(),
                input.tool_format.to_string(),
                "--requests".to_string(),
                "PROMOTION_REQUESTS.jsonl".to_string(),
                "--out".to_string(),
                "PROMOTION_BATCH.manifest.json".to_string(),
                "--id-prefix".to_string(),
                "lora-promotion".to_string(),
            ],
        },
        acceptance: PromotionAcceptance {
            required_metrics: input.required_metrics.to_vec(),
            gates: input.gates.to_vec(),
        },
    }
}

pub(super) fn lora_required_probe_cases(tool_format: &str) -> Vec<PromotionProbeCase> {
    let tool_surface = match tool_format {
        "native" => "provider-native structured tool call",
        "json" => "Harn fenced-JSON text tool-call block accepted by the parser",
        "text" => "Harn heredoc-capable text tool-call block accepted by the parser",
        _ => "Harn text tool-call block accepted by the catalog-selected parser",
    };
    vec![
        PromotionProbeCase {
            id: "sequential_tool_call".to_string(),
            requirement: "always".to_string(),
            expected: format!(
                "adapter-loaded route emits exactly one valid {tool_surface} with the requested tool name and arguments"
            ),
            receipt: "tool_probe_receipt.sequential_tool_call".to_string(),
            rationale: "catches the primary one-tool happy path before aggregate eval scores can hide parser drift"
                .to_string(),
        },
        PromotionProbeCase {
            id: "parallel_tool_calls".to_string(),
            requirement: "required_when_route_supports_parallel_tool_calls_with_route_capability_receipt"
                .to_string(),
            expected:
                "adapter-loaded route emits distinct tool calls with stable ids, names, and arguments when the route advertises parallel tools"
                    .to_string(),
            receipt: "tool_probe_receipt.parallel_tool_calls".to_string(),
            rationale:
                "prevents a LoRA from passing on sequential fixtures while breaking the advertised parallel contract"
                    .to_string(),
        },
        PromotionProbeCase {
            id: "no_tool_answer".to_string(),
            requirement: "always".to_string(),
            expected: "adapter-loaded route answers a non-tool prompt without emitting any tool call"
                .to_string(),
            receipt: "tool_probe_receipt.no_tool_answer".to_string(),
            rationale: "guards against over-triggered tool calls from narrow tool-call fine-tuning"
                .to_string(),
        },
        PromotionProbeCase {
            id: "unavailable_tool_repair".to_string(),
            requirement: "always".to_string(),
            expected:
                "adapter-loaded route recovers when the requested tool is absent instead of fabricating an unavailable tool call"
                    .to_string(),
            receipt: "tool_probe_receipt.unavailable_tool_repair".to_string(),
            rationale:
                "keeps tool selection grounded in the served schema rather than the training corpus inventory"
                    .to_string(),
        },
        PromotionProbeCase {
            id: "multi_turn_tool_result_continuation".to_string(),
            requirement: "always".to_string(),
            expected:
                "adapter-loaded route consumes a tool result and continues the same task without repeating or orphaning the prior call"
                    .to_string(),
            receipt: "tool_probe_receipt.multi_turn_tool_result_continuation".to_string(),
            rationale: "covers transcript lifecycle behavior that single-turn tool probes cannot observe"
                .to_string(),
        },
        PromotionProbeCase {
            id: "serving_concurrency_probe".to_string(),
            requirement: "required_for_adapter_loaded_serving_with_serving_receipt"
                .to_string(),
            expected:
                "adapter-loaded serving route preserves adapter binding, parser mode, and request ids across concurrent probe requests"
                    .to_string(),
            receipt: "tool_probe_receipt.serving_concurrency_probe".to_string(),
            rationale: "separates offline adapter quality from serving-path adapter and parser isolation"
                .to_string(),
        },
    ]
}

pub(super) struct PromotionEvidenceInput<'a> {
    pub(super) contract_id: &'a str,
    pub(super) base_model: &'a str,
    pub(super) provider: &'a str,
    pub(super) request_model: &'a str,
    pub(super) tool_format: &'a str,
    pub(super) eval_dataset: &'a str,
    pub(super) minimum_trials: u64,
    pub(super) required_metrics: &'a [String],
    pub(super) gates: &'a [String],
    pub(super) trainer_identity: Option<&'a TrainerIdentityCheck>,
    pub(super) trainer_environment: Option<&'a TrainerEnvironmentCheck>,
}

pub(super) fn lora_promotion_id(
    input: &PromotionEvidenceInput<'_>,
    required_probe_cases: &[PromotionProbeCase],
    probe_command_templates: &[PromotionProbeCommandTemplate],
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "harn_lora_promotion_v5",
        input.contract_id,
        input.base_model,
        input.provider,
        input.request_model,
        input.tool_format,
        input.eval_dataset,
        &input.minimum_trials.to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    if let Some(trainer_identity) = input.trainer_identity {
        let trainer_identity_bytes =
            serde_json::to_vec(trainer_identity).expect("trainer identity is JSON-serializable");
        hasher.update(trainer_identity_bytes);
    }
    hasher.update([0]);
    if let Some(trainer_environment) = input.trainer_environment {
        let trainer_environment_bytes = serde_json::to_vec(trainer_environment)
            .expect("trainer environment is JSON-serializable");
        hasher.update(trainer_environment_bytes);
    }
    hasher.update([0]);
    for metric in input.required_metrics {
        hasher.update(metric.as_bytes());
        hasher.update([0]);
    }
    for gate in input.gates {
        hasher.update(gate.as_bytes());
        hasher.update([0]);
    }
    let probe_case_bytes = serde_json::to_vec(required_probe_cases)
        .expect("promotion probe cases are JSON-serializable");
    hasher.update(probe_case_bytes);
    hasher.update([0]);
    let probe_command_bytes = serde_json::to_vec(probe_command_templates)
        .expect("promotion probe command templates are JSON-serializable");
    hasher.update(probe_command_bytes);
    hasher.update([0]);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(super) fn template_recipe_for_route(
    model_id: &str,
    family: &str,
    lineage: &str,
    tool_format: &str,
) -> TemplateRecipe {
    if tool_format == "native" && is_functiongemma_route(model_id, family, lineage) {
        return TemplateRecipe {
            name: "functiongemma_control_tokens".to_string(),
            source: "FunctionGemma declaration/call/response control-token template".to_string(),
            supervised_target: "model turn containing function-call control-token blocks"
                .to_string(),
            requirements: vec![
                "render function declarations, calls, and responses with FunctionGemma control tokens"
                    .to_string(),
                "treat <start_function_response> as an inference stop sequence".to_string(),
                "preserve string-value escaping with the model's escape delimiter".to_string(),
            ],
            stop_sequences: vec!["<start_function_response>".to_string()],
            notes: vec![
                "FunctionGemma is a specialized text-only function-calling model; do not mix this template with Harn <tool_call> text records"
                    .to_string(),
                "keep single-turn and multi-turn examples separated in eval so specialization does not hide turn-repair regressions"
                    .to_string(),
            ],
        };
    }
    if tool_format == "native" && is_gemma4_route(model_id, family, lineage) {
        return TemplateRecipe {
            name: "gemma4_native_function_calling".to_string(),
            source: "Gemma 4 tokenizer/provider native function-calling chat template".to_string(),
            supervised_target: "assistant messages with native tool_calls plus paired tool role results"
                .to_string(),
            requirements: vec![
                "use messages plus tools JSON schemas; let the tokenizer/provider render the Gemma 4 tool declaration syntax"
                    .to_string(),
                "train against the same native tool-call shape used at inference".to_string(),
                "do not include Harn <tool_call> text blocks in native Gemma 4 examples".to_string(),
            ],
            stop_sequences: Vec::new(),
            notes: vec![
                "Gemma 4 has native function-calling support, but local runtimes may still be catalog-steered to Harn text/json formats"
                    .to_string(),
                "if the route is served through Harn text/json, prefer the Harn template plan over the native Gemma 4 template"
                    .to_string(),
            ],
        };
    }
    match tool_format {
        "native" => TemplateRecipe {
            name: "native_messages_with_tools".to_string(),
            source: "tokenizer/provider chat template with tool schemas".to_string(),
            supervised_target: "assistant tool_calls and final assistant messages".to_string(),
            requirements: vec![
                "store examples as messages plus a tools column containing JSON schemas".to_string(),
                "represent tool results as tool role messages paired to assistant tool calls".to_string(),
                "verify the tokenizer chat template supports tool use before training".to_string(),
            ],
            stop_sequences: Vec::new(),
            notes: vec![
                "native adapters are portable only across runtimes that preserve the same chat template and tool schema rendering"
                    .to_string(),
            ],
        },
        "json" => TemplateRecipe {
            name: "harn_text_tool_calls_json_fences".to_string(),
            source: "Harn text tool-call parser using JSON object bodies".to_string(),
            supervised_target: "assistant_tool_text containing <tool_call>{\"name\":...,\"arguments\":...}</tool_call>"
                .to_string(),
            requirements: vec![
                "parse every assistant_tool_text example with Harn before training".to_string(),
                "keep tool definitions in the tools column and keep serialized calls byte-stable"
                    .to_string(),
                "reject markdown fences or model-native tool tags inside <tool_call> blocks"
                    .to_string(),
            ],
            stop_sequences: vec!["</tool_call>".to_string()],
            notes: vec![
                "this is the right target when the catalog steers a model to Harn's JSON text tool convention"
                    .to_string(),
            ],
        },
        "text" => TemplateRecipe {
            name: "harn_text_tool_calls_heredoc".to_string(),
            source: "Harn text tool-call parser using name({ ... }) and heredoc bodies".to_string(),
            supervised_target: "assistant_tool_text containing Harn text/heredoc <tool_call> blocks"
                .to_string(),
            requirements: vec![
                "parse every assistant_tool_text example with Harn before training".to_string(),
                "preserve heredoc boundaries for multiline edit/scaffold arguments".to_string(),
                "reject JSON object tool-call bodies unless the record declares the json lane"
                    .to_string(),
            ],
            stop_sequences: vec!["</tool_call>".to_string()],
            notes: vec![
                "this is the most direct adapter target for Burin's text tool-calling corpus"
                    .to_string(),
            ],
        },
        _ => TemplateRecipe {
            name: "route_validated_tool_template".to_string(),
            source: "catalog-validated route tool-call convention".to_string(),
            supervised_target: "assistant tool-call target selected by the effective route".to_string(),
            requirements: vec!["resolve the effective tool format before exporting examples".to_string()],
            stop_sequences: Vec::new(),
            notes: vec!["keep training and inference on the same route convention".to_string()],
        },
    }
}

pub(super) fn is_functiongemma_route(model_id: &str, family: &str, lineage: &str) -> bool {
    route_key(model_id, family, lineage).contains("functiongemma")
}

pub(super) fn is_gemma4_route(model_id: &str, family: &str, lineage: &str) -> bool {
    let key = route_key(model_id, family, lineage);
    key.contains("gemma-4") || key.contains("gemma4")
}

pub(super) fn route_key(model_id: &str, family: &str, lineage: &str) -> String {
    format!("{model_id} {family} {lineage}").to_ascii_lowercase()
}

pub(super) fn plan_warnings(
    provider: &str,
    decision: &harn_vm::llm::capabilities::ToolFormatDecision,
    provider_supports_lora_launch: bool,
    native_tools: bool,
    requested_tool_format: &str,
    requested_corpus_strategy: &str,
    effective_corpus_strategy: &str,
    teacher: Option<&TeacherReport>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    if requested_tool_format == "native" && decision.effective != "native" {
        warnings.push("native tool training requested but the catalog steered this route to a text-channel format".to_string());
    }
    if decision.effective == "native" && !native_tools {
        warnings.push(
            "effective tool format is native, but this route does not advertise native tools; use auto/text/json unless the serving proxy supplies native tools"
                .to_string(),
        );
    }
    if !provider_supports_lora_launch {
        warnings.push(format!(
            "provider {provider} does not declare local-runtime LoRA launch flags; plan still describes training and eval, but launch must be external"
        ));
    }
    if matches!(effective_corpus_strategy, "refresh" | "distill") && teacher.is_none() {
        warnings.push(format!(
            "corpus strategy {effective_corpus_strategy} needs --teacher to generate or repair examples"
        ));
    }
    if requested_corpus_strategy == "audit-only" && teacher.is_some() {
        warnings.push(
            "--teacher was supplied but corpus strategy is audit-only; teacher metadata is recorded but generation stays disabled"
                .to_string(),
        );
    }
    warnings
}
