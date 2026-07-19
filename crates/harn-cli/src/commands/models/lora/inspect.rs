use super::*;

pub(super) fn inspect_report(args: &ModelsLoraInspectArgs) -> Result<LoraInspectReport, String> {
    if args.require_contract_id && args.manifest.is_none() {
        return Err("--require-contract-id requires --manifest".to_string());
    }
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let provider = resolve_lora_provider(args.provider.as_deref(), &resolved.provider);
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &resolved.id);
    let tool_format = harn_vm::llm_config::default_tool_format(&resolved.id, &provider);
    let adapter = inspect_adapter(&args.adapter, args.name.as_deref())?;
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let provider_lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let provider_supports_lora_max_rank = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.max_lora_rank_arg.as_ref())
        .is_some();
    let base_model_match =
        base_model_match(adapter.base_model_name_or_path.as_deref(), &resolved.id);
    let mut warnings = Vec::new();
    if adapter.exists && !adapter.config_found {
        warnings.push("local adapter exists but adapter_config.json was not found".to_string());
    }
    if adapter.exists && adapter.weights_found.is_empty() {
        warnings.push("local adapter has no adapter_model.* weight file".to_string());
    }
    if adapter
        .peft_type
        .as_deref()
        .is_some_and(|peft| peft != "LORA")
    {
        warnings.push("adapter_config.json peft_type is not LORA".to_string());
    }
    if matches!(base_model_match, BaseModelMatch::Mismatch) {
        warnings.push(format!(
            "adapter base_model_name_or_path does not match resolved base model {}",
            resolved.id
        ));
    }
    if !adapter.exists {
        warnings.push(
            "adapter path does not exist locally; treating it as a remote/runtime-resolved id"
                .to_string(),
        );
    }
    if !provider_supports_lora_launch {
        warnings.push(format!(
            "provider {provider} does not declare local-runtime LoRA launch flags"
        ));
    }
    if adapter.rank.is_some() && provider_supports_lora_launch && !provider_supports_lora_max_rank {
        warnings.push(format!(
            "adapter rank is known but provider {provider} does not declare a max LoRA rank flag"
        ));
    }
    let contract = inspect_contract_report(
        args.manifest.as_deref(),
        args.require_contract_id,
        &adapter,
        &resolved.id,
        &provider,
        &tool_format,
    )?;
    if let Some(contract) = &contract {
        warnings.extend(contract.warnings.clone());
    }
    let ok = warnings.iter().all(|warning| {
        !warning.starts_with("local adapter exists")
            && !warning.starts_with("adapter_config.json peft_type")
            && !warning.starts_with("adapter base_model_name_or_path")
            && !warning.starts_with("LoRA contract mismatch")
            && !warning.starts_with("LoRA contract missing")
    });
    let request_model = adapter.name.clone();
    let max_lora_rank = adapter
        .rank
        .filter(|_| provider_supports_lora_launch && provider_supports_lora_max_rank);
    let harn_local_launch = if provider_supports_lora_launch {
        let model_source = adapter
            .base_model_name_or_path
            .clone()
            .unwrap_or_else(|| resolved.id.clone());
        let mut command = vec![
            "harn".to_string(),
            "local".to_string(),
            "launch".to_string(),
            args.base_model.clone(),
            "--provider".to_string(),
            provider.clone(),
            "--model-source".to_string(),
            model_source,
            "--lora-adapter".to_string(),
            format!("{}={}", adapter.name, adapter.input),
        ];
        if let Some(rank) = max_lora_rank {
            command.extend(["--max-lora-rank".to_string(), rank.to_string()]);
        }
        command
    } else {
        Vec::new()
    };
    let serving = InspectServingReport {
        request_model: request_model.clone(),
        base_model: resolved.id.clone(),
        provider: provider.clone(),
        tool_format: tool_format.clone(),
        lora_module_value_format: provider_lora_module_value_format.clone(),
        serving_requirements: tool_call_serving_requirements(&resolved.id, &provider, &tool_format),
    };
    Ok(LoraInspectReport {
        ok,
        base: BaseModelReport {
            selector: args.base_model.clone(),
            id: resolved.id.clone(),
            provider,
            resolved_alias: resolved.alias,
            tool_format,
            tier: resolved.tier,
            family: resolved.family,
            lineage: resolved.lineage,
            catalog_name: catalog.as_ref().map(|model| model.name.clone()),
            context_window: catalog.as_ref().map(|model| model.context_window),
        },
        adapter,
        contract,
        compatibility: CompatibilityReport {
            base_model_match,
            provider_supports_lora_launch,
            provider_supports_lora_max_rank,
            provider_lora_module_value_format,
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        serving,
        launch: LaunchHints {
            request_model,
            max_lora_rank,
            harn_local_launch,
        },
        warnings,
    })
}

pub(super) fn inspect_contract_report(
    manifest_path: Option<&Path>,
    require_adapter_contract_id: bool,
    adapter: &AdapterReport,
    resolved_base_model: &str,
    provider: &str,
    tool_format: &str,
) -> Result<Option<InspectContractReport>, String> {
    let Some(path) = manifest_path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read manifest {}: {error}", path.display()))?;
    let manifest = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|error| format!("failed to parse manifest {}: {error}", path.display()))?;
    let contract = manifest
        .get("contract")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("manifest {} is missing contract object", path.display()))?;
    let contract_id = manifest_string_from_object(contract, "id");
    let manifest_base_model = manifest_string_from_object(contract, "base_model");
    let manifest_provider = manifest_string_from_object(contract, "provider");
    let manifest_tool_format = manifest_string_from_object(contract, "harn_tool_format");
    let manifest_dataset_format = manifest_string_from_object(contract, "dataset_format");
    let manifest_chat_template = manifest_string_from_object(contract, "chat_template");
    let manifest_target_modules = manifest_target_modules_from_contract(contract);
    let manifest_modules_to_save = manifest_modules_to_save_from_contract(contract);
    let target_adapter_name = manifest
        .get("target")
        .and_then(serde_json::Value::as_object)
        .and_then(|target| manifest_string_from_object(target, "adapter_name"));
    let serving_request_model = manifest
        .get("serving")
        .and_then(serde_json::Value::as_object)
        .and_then(|serving| manifest_string_from_object(serving, "request_model"));

    let base_model_match = base_model_match(manifest_base_model.as_deref(), resolved_base_model);
    let provider_matches = manifest_provider
        .as_deref()
        .is_some_and(|manifest_provider| manifest_provider == provider);
    let tool_format_matches = manifest_tool_format
        .as_deref()
        .is_some_and(|manifest_tool_format| manifest_tool_format == tool_format);
    let adapter_name_expectations = [
        target_adapter_name.as_deref(),
        serving_request_model.as_deref(),
    ];
    let adapter_name_matches = if adapter_name_expectations.iter().any(Option::is_some) {
        Some(
            adapter_name_expectations
                .into_iter()
                .flatten()
                .all(|expected| expected == adapter.name),
        )
    } else {
        None
    };
    let adapter_contract_id_matches = match (&adapter.contract_id, &contract_id) {
        (Some(adapter_id), Some(manifest_id)) => Some(adapter_id == manifest_id),
        _ => None,
    };
    let adapter_modules_to_save = normalize_modules_to_save_lossy(adapter.modules_to_save.clone());
    let adapter_target_modules = normalize_target_modules(&adapter.target_modules)
        .unwrap_or_else(|_| adapter.target_modules.clone());
    let target_modules_match = manifest_target_modules
        .as_ref()
        .map(|manifest_target| manifest_target.modules == adapter_target_modules);
    let modules_to_save_matches = manifest_modules_to_save
        .as_ref()
        .map(|manifest_modules| manifest_modules == &adapter_modules_to_save);

    let mut warnings = Vec::new();
    if matches!(
        base_model_match,
        BaseModelMatch::Mismatch | BaseModelMatch::Unknown
    ) {
        warnings.push(format!(
            "LoRA contract mismatch: manifest base_model={} does not match resolved base {}",
            manifest_base_model.as_deref().unwrap_or("<missing>"),
            resolved_base_model
        ));
    }
    if !provider_matches {
        warnings.push(format!(
            "LoRA contract mismatch: manifest provider={} does not match provider {}",
            manifest_provider.as_deref().unwrap_or("<missing>"),
            provider
        ));
    }
    if !tool_format_matches {
        warnings.push(format!(
            "LoRA contract mismatch: manifest tool format={} does not match route tool format {}",
            manifest_tool_format.as_deref().unwrap_or("<missing>"),
            tool_format
        ));
    }
    if contract_id.is_none() {
        warnings.push("LoRA contract mismatch: manifest contract.id is missing".to_string());
    }
    if adapter_name_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: manifest adapter/request model does not match adapter name {}",
            adapter.name
        ));
    }
    if adapter_contract_id_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: adapter contract id {} does not match manifest contract id {}",
            adapter.contract_id.as_deref().unwrap_or("<missing>"),
            contract_id.as_deref().unwrap_or("<missing>")
        ));
    }
    if manifest_modules_to_save.is_none() {
        warnings.push("LoRA contract mismatch: manifest PEFT save policy is missing".to_string());
    } else if modules_to_save_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: adapter modules_to_save {:?} does not match manifest modules_to_save {:?}",
            adapter_modules_to_save,
            manifest_modules_to_save.as_deref().unwrap_or(&[])
        ));
    }
    if manifest_target_modules.is_none() {
        warnings
            .push("LoRA contract mismatch: manifest target-module contract is missing".to_string());
    } else if target_modules_match == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: adapter target_modules {:?} does not match manifest target_modules {:?}",
            adapter_target_modules,
            manifest_target_modules
                .as_ref()
                .map(|contract| contract.modules.as_slice())
                .unwrap_or(&[])
        ));
    }
    if adapter.contract_id.is_none() {
        let prefix = if require_adapter_contract_id {
            "LoRA contract missing"
        } else {
            "LoRA contract warning"
        };
        warnings.push(format!(
            "{prefix}: adapter_config.json does not include harn_lora_contract_id"
        ));
    }

    let status = if warnings.iter().any(|warning| {
        warning.starts_with("LoRA contract mismatch")
            || warning.starts_with("LoRA contract missing")
    }) {
        ContractCheckStatus::Fail
    } else if warnings.is_empty() {
        ContractCheckStatus::Pass
    } else {
        ContractCheckStatus::Warn
    };

    Ok(Some(InspectContractReport {
        manifest_path: path.display().to_string(),
        contract_id,
        adapter_contract_id: adapter.contract_id.clone(),
        status,
        base_model_match,
        provider_matches,
        tool_format_matches,
        adapter_name_matches,
        target_modules_match,
        modules_to_save_matches,
        require_adapter_contract_id,
        manifest: InspectContractManifest {
            base_model: manifest_base_model,
            provider: manifest_provider,
            harn_tool_format: manifest_tool_format,
            dataset_format: manifest_dataset_format,
            chat_template: manifest_chat_template,
            target_modules: manifest_target_modules,
            modules_to_save: manifest_modules_to_save,
            adapter_name: target_adapter_name,
            request_model: serving_request_model,
        },
        warnings,
    }))
}

pub(super) fn manifest_target_modules_from_contract(
    contract: &serde_json::Map<String, serde_json::Value>,
) -> Option<TargetModuleContract> {
    let target = contract.get("target_modules")?.as_object()?;
    let policy = manifest_string_from_object(target, "policy")?;
    let modules = normalize_target_modules(&value_string_list(target.get("modules")?)).ok()?;
    Some(TargetModuleContract { policy, modules })
}

pub(super) fn manifest_modules_to_save_from_contract(
    contract: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<String>> {
    let modules = contract
        .get("training_contract")?
        .get("peft_save_policy")?
        .get("modules_to_save")?;
    Some(normalize_modules_to_save_lossy(value_string_list(modules)))
}

pub(super) fn manifest_string_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
