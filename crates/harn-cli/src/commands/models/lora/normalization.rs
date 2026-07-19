use super::*;

pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn inspect_adapter(
    input: &str,
    explicit_name: Option<&str>,
) -> Result<AdapterReport, String> {
    let expanded = expand_home(input);
    let path = PathBuf::from(&expanded);
    let exists = path.exists();
    let adapter_dir = if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "adapter_config.json")
    {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path
    };
    let config_path = adapter_dir.join("adapter_config.json");
    let config_found = config_path.is_file();
    let config = if config_found {
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
        Some(
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|error| format!("failed to parse {}: {error}", config_path.display()))?,
        )
    } else {
        None
    };
    let weights_found = adapter_weights(&adapter_dir);
    Ok(AdapterReport {
        input: input.to_string(),
        name: explicit_name
            .map(str::to_string)
            .unwrap_or_else(|| adapter_name_from_input(input)),
        local_path: exists.then(|| adapter_dir.display().to_string()),
        exists,
        config_found,
        config_path: config_found.then(|| config_path.display().to_string()),
        weights_found,
        peft_type: config_string(&config, "peft_type"),
        task_type: config_string(&config, "task_type"),
        base_model_name_or_path: config_string(&config, "base_model_name_or_path"),
        rank: config_u64(&config, "r"),
        lora_alpha: config_f64(&config, "lora_alpha"),
        target_modules: config_string_list(&config, "target_modules"),
        modules_to_save: normalize_modules_to_save_lossy(config_string_list(
            &config,
            "modules_to_save",
        )),
        contract_id: config_contract_id(&config),
    })
}

pub(super) fn adapter_weights(dir: &Path) -> Vec<String> {
    ["adapter_model.safetensors", "adapter_model.bin"]
        .into_iter()
        .filter_map(|name| {
            let path = dir.join(name);
            path.is_file().then(|| path.display().to_string())
        })
        .collect()
}

pub(super) fn config_string(config: &Option<serde_json::Value>, key: &str) -> Option<String> {
    config.as_ref()?.get(key)?.as_str().map(str::to_string)
}

pub(super) fn config_u64(config: &Option<serde_json::Value>, key: &str) -> Option<u64> {
    config.as_ref()?.get(key)?.as_u64()
}

pub(super) fn config_f64(config: &Option<serde_json::Value>, key: &str) -> Option<f64> {
    let value = config.as_ref()?.get(key)?;
    value.as_f64().or_else(|| value.as_u64().map(|n| n as f64))
}

pub(super) fn config_string_list(config: &Option<serde_json::Value>, key: &str) -> Vec<String> {
    let Some(value) = config.as_ref().and_then(|value| value.get(key)) else {
        return Vec::new();
    };
    value_string_list(value)
}

pub(super) fn value_string_list(value: &serde_json::Value) -> Vec<String> {
    if let Some(text) = value.as_str() {
        return vec![text.to_string()];
    }
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn config_contract_id(config: &Option<serde_json::Value>) -> Option<String> {
    [
        "harn_lora_contract_id",
        "lora_contract_id",
        "harn_contract_id",
    ]
    .into_iter()
    .find_map(|key| config_string(config, key))
}

pub(super) fn base_model_match(declared: Option<&str>, resolved_id: &str) -> BaseModelMatch {
    let Some(declared) = declared.map(str::trim).filter(|value| !value.is_empty()) else {
        return BaseModelMatch::Unknown;
    };
    let declared = normalize_model_name(declared);
    let resolved = normalize_model_name(resolved_id);
    if declared == resolved {
        return BaseModelMatch::Exact;
    }
    let declared_tail = declared.rsplit('/').next().unwrap_or(&declared);
    let resolved_tail = resolved.rsplit('/').next().unwrap_or(&resolved);
    if declared_tail == resolved_tail {
        BaseModelMatch::Suffix
    } else {
        BaseModelMatch::Mismatch
    }
}

pub(super) fn normalize_model_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("models/")
        .to_ascii_lowercase()
}

pub(super) fn adapter_name_from_input(input: &str) -> String {
    input
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("lora-adapter")
        .to_string()
}

pub(super) fn expand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    value.to_string()
}

pub(super) fn normalize_lora_method(raw: &str) -> Result<String, String> {
    let method = raw.trim().to_ascii_lowercase();
    match method.as_str() {
        "lora" | "qlora" => Ok(method),
        _ => Err(format!(
            "unsupported LoRA method `{raw}`; expected `qlora` or `lora`"
        )),
    }
}

pub(super) fn resolve_lora_provider(provider: Option<&str>, resolved_provider: &str) -> String {
    provider
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(normalize_local_provider_id)
        .unwrap_or_else(|| normalize_local_provider_id(resolved_provider))
}

pub(super) fn normalize_lora_rank(rank: u32) -> Result<u32, String> {
    if rank == 0 {
        return Err("--rank must be greater than 0".to_string());
    }
    Ok(rank)
}

pub(super) fn normalize_lora_alpha(alpha: Option<u32>, rank: u32) -> Result<u32, String> {
    let alpha = alpha.unwrap_or_else(|| rank.saturating_mul(2));
    if alpha == 0 {
        return Err("--alpha must be greater than 0".to_string());
    }
    Ok(alpha)
}

pub(super) fn normalize_lora_dropout(dropout: f64) -> Result<f64, String> {
    if !dropout.is_finite() || !(0.0..1.0).contains(&dropout) {
        return Err("--dropout must be a finite value in [0.0, 1.0)".to_string());
    }
    Ok(dropout)
}

pub(super) fn target_modules_for_route(
    method: &str,
    model_id: &str,
    family: &str,
    lineage: &str,
) -> Vec<String> {
    match method {
        "qlora" => vec!["all-linear".to_string()],
        _ if is_gemma4_route(model_id, family, lineage) => vec![
            "q_proj".to_string(),
            "k_proj".to_string(),
            "v_proj".to_string(),
            "o_proj".to_string(),
            "gate_proj".to_string(),
            "up_proj".to_string(),
            "down_proj".to_string(),
        ],
        _ => vec![
            "q_proj".to_string(),
            "k_proj".to_string(),
            "v_proj".to_string(),
            "o_proj".to_string(),
        ],
    }
}

pub(super) fn target_module_contract(
    raw: &[String],
    method: &str,
    model_id: &str,
    family: &str,
    lineage: &str,
) -> Result<TargetModuleContract, String> {
    let modules = normalize_target_modules(raw)?;
    if !modules.is_empty() {
        return Ok(TargetModuleContract {
            policy: "explicit".to_string(),
            modules,
        });
    }
    let modules = target_modules_for_route(method, model_id, family, lineage);
    Ok(TargetModuleContract {
        policy: if modules == ["all-linear"] {
            "all_linear"
        } else {
            "route_default"
        }
        .to_string(),
        modules,
    })
}

pub(super) fn normalize_target_modules(raw: &[String]) -> Result<Vec<String>, String> {
    normalize_module_list(raw, "--target-modules")
}

pub(super) fn normalize_modules_to_save(raw: &[String]) -> Result<Vec<String>, String> {
    normalize_module_list(raw, "--modules-to-save")
}

pub(super) fn normalize_module_list(raw: &[String], flag: &str) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    for item in raw {
        for piece in item.split(',') {
            let module = piece.trim();
            if module.is_empty() {
                return Err(format!("{flag} entries must not be empty"));
            }
            seen.insert(module.to_string());
        }
    }
    Ok(seen.into_iter().collect())
}

pub(super) fn target_modules_args(contract: &TargetModuleContract) -> Vec<String> {
    contract
        .modules
        .iter()
        .flat_map(|module| ["--target-modules".to_string(), module.clone()])
        .collect()
}

pub(super) fn normalize_modules_to_save_lossy(raw: Vec<String>) -> Vec<String> {
    normalize_modules_to_save(&raw).unwrap_or(raw)
}

pub(super) fn modules_to_save_args(modules: &[String]) -> Vec<String> {
    modules
        .iter()
        .flat_map(|module| ["--modules-to-save".to_string(), module.clone()])
        .collect()
}

pub(super) fn normalize_plan_tool_format(raw: &str) -> Result<String, String> {
    let tool_format = raw.trim().to_ascii_lowercase();
    match tool_format.as_str() {
        "auto" | "native" | "text" | "json" => Ok(tool_format),
        _ => Err(format!(
            "unsupported tool format `{raw}`; expected `auto`, `native`, `text`, or `json`"
        )),
    }
}

pub(super) fn normalize_corpus_strategy(raw: &str) -> Result<String, String> {
    let strategy = raw.trim().to_ascii_lowercase();
    match strategy.as_str() {
        "auto" | "audit-only" | "refresh" | "distill" => Ok(strategy),
        _ => Err(format!(
            "unsupported corpus strategy `{raw}`; expected `auto`, `audit-only`, `refresh`, or `distill`"
        )),
    }
}

pub(super) fn normalize_tool_catalog_policy(raw: &str) -> Result<String, String> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "" | "full_schema" | "full_tool_schema" | "full_tool_schemas" => {
            Ok("full_schema".to_string())
        }
        "compressed_names" | "names_only" | "tool_names_only" | "compressed_tool_names" => {
            Ok("compressed_names".to_string())
        }
        "fixed_catalog_internalized" | "internalized_fixed_catalog" | "internalized"
        | "no_catalog" | "none" => Ok("fixed_catalog_internalized".to_string()),
        other => Err(format!(
            "unsupported tool catalog policy `{other}`; expected `full_schema`, `compressed_names`, or `fixed_catalog_internalized`"
        )),
    }
}

pub(super) fn normalize_optional_catalog_identity(
    raw: Option<&str>,
    flag: &str,
) -> Result<Option<String>, String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.contains(char::is_whitespace) {
                Err(format!("{flag} must not contain whitespace"))
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

pub(super) fn tool_catalog_contract(
    policy: &str,
    catalog_id: Option<&str>,
    catalog_hash: Option<&str>,
) -> Result<ToolCatalogContract, String> {
    let catalog_id = normalize_optional_catalog_identity(catalog_id, "--tool-catalog-id")?;
    let catalog_hash = normalize_optional_catalog_identity(catalog_hash, "--tool-catalog-hash")?;
    if policy != "full_schema" && catalog_id.is_none() && catalog_hash.is_none() {
        return Err(format!(
            "--tool-catalog-policy {policy} requires --tool-catalog-id or --tool-catalog-hash so the fixed catalog is auditable"
        ));
    }
    let (training_catalog, inference_catalog, prompt_catalog_requirement, notes) = match policy {
        "full_schema" => (
            "full_json_schema",
            "full_json_schema",
            "include full tool schemas at inference",
            vec![
                "default production route: prompts keep the same full tool schemas used to validate the dataset".to_string(),
                "safe for changing tool catalogs as long as each prompt carries the current schema set".to_string(),
            ],
        ),
        "compressed_names" => (
            "full_json_schema_for_validation",
            "compressed_tool_names_only",
            "include the fixed tool names only; omit argument schemas from the prompt",
            vec![
                "experiment route: adapter must infer argument schemas from a declared fixed catalog".to_string(),
                "promotion must compare against the full-schema baseline before this route is used outside controlled evals".to_string(),
            ],
        ),
        "fixed_catalog_internalized" => (
            "fixed_full_json_schema_for_training",
            "no_runtime_catalog",
            "omit runtime tool catalog; adapter weights are bound to the declared fixed catalog",
            vec![
                "experiment route: any tool addition, deletion, rename, or schema change creates a new adapter contract".to_string(),
                "do not use as the default production route without adapter-loaded promotion receipts for the exact fixed catalog".to_string(),
            ],
        ),
        _ => unreachable!("normalize_tool_catalog_policy returned an unsupported policy"),
    };
    Ok(ToolCatalogContract {
        schema_version: LORA_TOOL_CATALOG_CONTRACT_SCHEMA_VERSION,
        policy: policy.to_string(),
        catalog_id,
        catalog_hash,
        training_catalog: training_catalog.to_string(),
        inference_catalog: inference_catalog.to_string(),
        schema_columns_required: true,
        prompt_catalog_requirement: prompt_catalog_requirement.to_string(),
        notes,
        promotion_gates: vec![
            "record the exact catalog policy and catalog identity in export, train, manifest, and promotion receipts".to_string(),
            "compare compressed/no-catalog adapters against a full-schema baseline on the same frozen tool cases".to_string(),
            "rerun promotion when any tool name, argument schema, catalog id, catalog hash, or prompt catalog policy changes".to_string(),
        ],
    })
}

pub(super) fn tool_catalog_args(contract: &ToolCatalogContract) -> Vec<String> {
    let mut args = Vec::new();
    if contract.policy != "full_schema" {
        args.extend(["--tool-catalog-policy".to_string(), contract.policy.clone()]);
    }
    if let Some(catalog_id) = &contract.catalog_id {
        args.extend(["--tool-catalog-id".to_string(), catalog_id.clone()]);
    }
    if let Some(catalog_hash) = &contract.catalog_hash {
        args.extend(["--tool-catalog-hash".to_string(), catalog_hash.clone()]);
    }
    args
}

pub(super) fn parse_target_metadata(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut metadata = BTreeMap::new();
    for item in raw {
        let Some((key, value)) = item.split_once('=') else {
            return Err(format!(
                "invalid --target-metadata `{item}`; expected KEY=VALUE"
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("invalid --target-metadata `{item}`; key is empty"));
        }
        metadata.insert(key.to_string(), value.to_string());
    }
    Ok(metadata)
}

pub(super) fn effective_corpus_strategy(
    requested: &str,
    corpus: Option<&str>,
    teacher: Option<&TeacherReport>,
) -> String {
    if requested != "auto" {
        return requested.to_string();
    }
    if teacher.is_none() {
        return "audit-only".to_string();
    }
    if corpus.is_some() {
        "refresh".to_string()
    } else {
        "distill".to_string()
    }
}

pub(super) fn quantization_for_method(method: &str) -> &'static str {
    match method {
        "qlora" => "4bit_base_model",
        "lora" => "base_model_precision",
        _ => unreachable!("normalize_lora_method returned an unsupported method"),
    }
}

pub(super) fn precision_contract_for_method(method: &str) -> PrecisionContract {
    let (
        training_base_precision,
        serving_base_precision,
        compatibility_policy,
    ) = match method {
        "qlora" => (
            "4bit_nf4_or_runtime_equivalent",
            "same_quantization_family_as_training_or_revalidate",
            "changing the base quantization or compute dtype makes a new route until promotion gates pass",
        ),
        "lora" => (
            "base_model_precision",
            "same_base_model_precision_as_training_or_revalidate",
            "changing the base or adapter precision makes a new route until promotion gates pass",
        ),
        _ => unreachable!("normalize_lora_method returned an unsupported method"),
    };
    PrecisionContract {
        schema_version: 1,
        training_base_precision: training_base_precision.to_string(),
        training_compute_precision: "bf16_when_supported_else_fp16".to_string(),
        adapter_weight_precision: "bf16_or_fp16_lora_weights".to_string(),
        serving_base_precision: serving_base_precision.to_string(),
        serving_adapter_precision: "load_adapter_weights_without_merge_until_promotion".to_string(),
        compatibility_policy: compatibility_policy.to_string(),
        promotion_gates: vec![
            "record training base precision, compute precision, adapter precision, and serving base precision in the adapter manifest".to_string(),
            "compare base versus adapter using the same base precision planned for serving".to_string(),
            "rerun promotion gates whenever quantization, compute dtype, chat template, or tool format changes".to_string(),
        ],
    }
}

pub(super) fn precision_target_metadata(precision: &PrecisionContract) -> Vec<String> {
    [
        (
            "training_base_precision",
            precision.training_base_precision.as_str(),
        ),
        (
            "training_compute_precision",
            precision.training_compute_precision.as_str(),
        ),
        (
            "adapter_weight_precision",
            precision.adapter_weight_precision.as_str(),
        ),
        (
            "serving_base_precision",
            precision.serving_base_precision.as_str(),
        ),
    ]
    .into_iter()
    .flat_map(|(key, value)| ["--target-metadata".to_string(), format!("{key}={value}")])
    .collect()
}

pub(super) fn merge_serving_target_metadata(
    metadata: &mut BTreeMap<String, String>,
    serving: &ServingRecipe,
    warnings: &mut Vec<String>,
) {
    for (key, value) in serving_target_metadata(serving) {
        if let Some(existing) = metadata.get(&key) {
            if existing != &value {
                warnings.push(format!(
                    "--target-metadata {key}={existing} overrides Harn-derived serving metadata {key}={value}; verify the manifest records the actual serving route"
                ));
            }
        } else {
            metadata.insert(key, value);
        }
    }
}

pub(super) fn target_metadata_args_from_map(metadata: &BTreeMap<String, String>) -> Vec<String> {
    metadata
        .iter()
        .flat_map(|(key, value)| ["--target-metadata".to_string(), format!("{key}={value}")])
        .collect()
}

pub(super) fn serving_target_metadata(serving: &ServingRecipe) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "serving_adapter_binding".to_string(),
        serving.adapter_binding.clone(),
    );
    metadata.insert(
        "serving_lora_modules_value_format".to_string(),
        serving.lora_module_value_format.clone(),
    );
    metadata.insert("serving_provider".to_string(), serving.provider.clone());
    metadata.insert(
        "serving_request_model".to_string(),
        serving.request_model.clone(),
    );
    metadata.insert(
        "tool_catalog_policy".to_string(),
        serving.tool_catalog.policy.clone(),
    );
    if let Some(catalog_id) = &serving.tool_catalog.catalog_id {
        metadata.insert("tool_catalog_id".to_string(), catalog_id.clone());
    }
    if let Some(catalog_hash) = &serving.tool_catalog.catalog_hash {
        metadata.insert("tool_catalog_hash".to_string(), catalog_hash.clone());
    }
    for requirement in &serving.serving_requirements {
        match (
            requirement.kind.as_str(),
            requirement.name.as_str(),
            requirement.value.as_deref(),
        ) {
            ("parser_owner", "tool_call_parser", Some(value)) => {
                metadata.insert("serving_tool_parser_owner".to_string(), value.to_string());
            }
            ("provider_native_tool_parser", "native_tool_parser_mode", Some(value)) => {
                metadata.insert("provider_native_tool_parser".to_string(), value.to_string());
            }
            ("server_flag", "--enable-auto-tool-choice", _) => {
                metadata.insert("vllm_auto_tool_choice".to_string(), "required".to_string());
            }
            ("server_flag", "--tool-call-parser", Some(value)) => {
                metadata.insert("tool_parser_id".to_string(), value.to_string());
            }
            ("server_flag", "--reasoning-parser", Some(value)) => {
                metadata.insert("reasoning_parser".to_string(), value.to_string());
            }
            ("chat_template", "chat_template", Some(value)) => {
                metadata.insert("serving_chat_template".to_string(), value.to_string());
            }
            ("stop_sequence", "inference_stop_sequence", Some(value)) => {
                metadata.insert("inference_stop_sequence".to_string(), value.to_string());
            }
            ("manifest_metadata", "tool_parser_id", Some(value)) => {
                metadata.insert("tool_parser_id".to_string(), value.to_string());
            }
            ("manifest_metadata", "chat_template_hash", _) => {
                metadata.insert(
                    "chat_template_hash_requirement".to_string(),
                    "required_after_rendering".to_string(),
                );
            }
            ("promotion_gate", "parser_concurrency_policy", Some(value)) => {
                metadata.insert("parser_concurrency_policy".to_string(), value.to_string());
            }
            _ => {}
        }
    }
    metadata
}
