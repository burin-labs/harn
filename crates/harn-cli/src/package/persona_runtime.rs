use super::*;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRuntimePersona {
    pub id: String,
    pub persona: PersonaManifestEntry,
    pub execution_policy: harn_vm::orchestration::CapabilityPolicy,
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
}

pub(crate) fn resolve_runtime_personas(
    manifest: Manifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
) -> Result<Vec<ResolvedRuntimePersona>, PackageError> {
    let root =
        validate_and_resolve_personas(manifest, manifest_path, manifest_dir).map_err(|errors| {
            PackageError::Manifest(
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
    let mut resolved = root
        .personas
        .iter()
        .map(|persona| {
            let execution_policy = persona_execution_policy(persona);
            ResolvedRuntimePersona {
                id: persona
                    .name
                    .clone()
                    .expect("validated persona has a required name"),
                persona: persona.clone(),
                execution_policy,
                manifest_path: root.manifest_path.clone(),
                manifest_dir: root.manifest_dir.clone(),
            }
        })
        .collect::<Vec<_>>();

    if root.manifest_path.file_name() == Some(OsStr::new(MANIFEST)) {
        let ledger = load_activation_ledger(&root.manifest_dir)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        for activation in ledger.activations.into_values() {
            let discovered = resolve_discoverable_persona_in_root(&root, &activation.persona_id)
                .map_err(PackageError::Manifest)?;
            let execution_policy = capability_policy(
                &activation.effective_policy.tools,
                &activation.effective_policy.capabilities,
            );
            let persona = materialize_activated_persona(&discovered, &activation)
                .map_err(|error| PackageError::Manifest(error.to_string()))?;
            resolved.push(ResolvedRuntimePersona {
                id: discovered.id,
                persona,
                execution_policy,
                manifest_path: discovered.manifest_path,
                manifest_dir: discovered.manifest_dir,
            });
        }
    }
    resolved.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(resolved)
}

pub(crate) fn persona_runtime_handler_for_trigger(
    extensions: &RuntimeExtensions,
    trigger: &ResolvedTriggerConfig,
    name: &str,
) -> Result<(harn_vm::PersonaRuntimeBinding, harn_vm::VmCallable), PackageError> {
    let Some(resolved) = extensions
        .runtime_personas
        .iter()
        .find(|resolved| resolved.id == name)
    else {
        return Err(trigger_error(
            trigger,
            format!("handler persona://{name} does not match an active persona"),
        ));
    };
    let callable = persona_runtime_callable(name, &resolved.persona, &resolved.manifest_dir)
        .map_err(|error| trigger_error(trigger, error.to_string()))?;
    Ok((
        persona_runtime_binding_with_policy(name, &resolved.persona, &resolved.execution_policy),
        callable,
    ))
}

pub fn collect_persona_trigger_binding_specs(
    extensions: &RuntimeExtensions,
) -> Result<Vec<harn_vm::TriggerBindingSpec>, PackageError> {
    let mut bindings = Vec::new();
    for resolved in &extensions.runtime_personas {
        for trigger in &resolved.persona.triggers {
            let (provider, kind) = trigger
                .split_once('.')
                .expect("validated persona trigger has provider.event form");
            let provider = provider.trim();
            let kind = kind.trim();
            bindings.push(persona_trigger_binding_spec(resolved, provider, kind)?);
        }
    }
    Ok(bindings)
}

fn persona_trigger_binding_spec(
    resolved: &ResolvedRuntimePersona,
    provider: &str,
    kind: &str,
) -> Result<harn_vm::TriggerBindingSpec, PackageError> {
    let runtime_binding = persona_runtime_binding_with_policy(
        &resolved.id,
        &resolved.persona,
        &resolved.execution_policy,
    );
    let callable =
        persona_runtime_callable(&resolved.id, &resolved.persona, &resolved.manifest_dir)?;
    let id = format!("persona.{}.{provider}.{kind}", resolved.id);
    let handler = harn_vm::TriggerHandlerSpec::Persona {
        binding: runtime_binding.clone(),
        callable,
    };
    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": &id,
        "kind": kind,
        "provider": provider,
        "handler": {
            "kind": "persona",
            "name": &resolved.id,
            "entry_workflow": runtime_binding.entry_workflow,
        },
        "budget": runtime_binding.budget,
        "manifest_path": resolved.manifest_path,
    }))
    .unwrap_or_else(|_| format!("{id}:{provider}:{kind}:{}", resolved.id));

    Ok(harn_vm::TriggerBindingSpec {
        id,
        source: harn_vm::TriggerBindingSource::Manifest,
        kind: kind.to_string(),
        provider: harn_vm::ProviderId::from(provider.to_string()),
        autonomy_tier: runtime_binding.autonomy_tier,
        handler,
        dispatch_priority: harn_vm::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: harn_vm::TriggerRetryConfig::default(),
        match_events: vec![kind.to_string()],
        dedupe_key: None,
        filter: None,
        dedupe_retention_days: 7,
        daily_cost_usd: resolved.persona.budget.daily_usd,
        hourly_cost_usd: resolved.persona.budget.hourly_usd,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::RetryLater,
        max_concurrent: None,
        flow_control: harn_vm::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: Some(resolved.manifest_path.clone()),
        package_name: None,
        definition_fingerprint: fingerprint,
    })
}

/// Lower a manifest persona entry into the runtime binding shared by CLI and triggers.
pub(crate) fn persona_runtime_binding(
    name: &str,
    persona: &PersonaManifestEntry,
) -> harn_vm::PersonaRuntimeBinding {
    persona_runtime_binding_with_policy(name, persona, &persona_execution_policy(persona))
}

fn persona_runtime_binding_with_policy(
    name: &str,
    persona: &PersonaManifestEntry,
    execution_policy: &harn_vm::orchestration::CapabilityPolicy,
) -> harn_vm::PersonaRuntimeBinding {
    harn_vm::PersonaRuntimeBinding {
        name: name.to_string(),
        autonomy_tier: persona
            .autonomy_tier
            .map(persona_autonomy_to_vm)
            .unwrap_or(harn_vm::AutonomyTier::Suggest),
        execution_policy: Box::new(execution_policy.clone()),
        template_ref: persona_template_ref(persona),
        entry_workflow: persona.entry_workflow.clone().unwrap_or_default(),
        schedules: persona.schedules.clone(),
        triggers: persona.triggers.clone(),
        budget: harn_vm::PersonaBudgetPolicy {
            daily_usd: persona.budget.daily_usd,
            hourly_usd: persona.budget.hourly_usd,
            run_usd: persona.budget.run_usd,
            max_tokens: persona.budget.max_tokens,
        },
        stages: persona
            .stages
            .iter()
            .map(persona_stage_decl_to_runtime)
            .collect(),
    }
}

fn persona_execution_policy(
    persona: &PersonaManifestEntry,
) -> harn_vm::orchestration::CapabilityPolicy {
    capability_policy(&persona.tools, &normalized_persona_capabilities(persona))
}

fn capability_policy(
    tools: &[String],
    persona_capabilities: &[String],
) -> harn_vm::orchestration::CapabilityPolicy {
    let mut capabilities = BTreeMap::new();
    for capability in persona_capabilities {
        let (name, operation) = capability
            .split_once('.')
            .expect("validated persona capability has capability.operation form");
        capabilities
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(operation.to_string());
    }
    harn_vm::orchestration::CapabilityPolicy {
        tools: tools.to_vec(),
        tools_restricted: true,
        capabilities,
        capabilities_restricted: true,
        ..Default::default()
    }
}

pub(crate) fn persona_autonomy_to_vm(value: PersonaAutonomyTier) -> harn_vm::AutonomyTier {
    match value {
        PersonaAutonomyTier::Shadow => harn_vm::AutonomyTier::Shadow,
        PersonaAutonomyTier::Suggest => harn_vm::AutonomyTier::Suggest,
        PersonaAutonomyTier::ActWithApproval => harn_vm::AutonomyTier::ActWithApproval,
        PersonaAutonomyTier::ActAuto => harn_vm::AutonomyTier::ActAuto,
    }
}

pub(crate) fn persona_runtime_callable(
    name: &str,
    persona: &PersonaManifestEntry,
    manifest_dir: &Path,
) -> Result<harn_vm::VmCallable, PackageError> {
    let entry_workflow = persona.entry_workflow.as_deref().ok_or_else(|| {
        PackageError::Manifest(format!("persona '{name}' is missing entry_workflow"))
    })?;
    let (module_path, function_name) = entry_workflow.split_once('#').ok_or_else(|| {
        PackageError::Manifest(format!(
            "persona '{name}' entry_workflow must be <module.harn>#<function>"
        ))
    })?;
    if !module_path.ends_with(".harn") || !valid_identifier(function_name) {
        return Err(PackageError::Manifest(format!(
            "persona '{name}' entry_workflow must be <module.harn>#<function>"
        )));
    }
    let module_path = safe_package_relative_path(manifest_dir, module_path)?;
    let signatures = load_module_callable_signatures(&module_path)?;
    if signatures
        .get(function_name)
        .is_none_or(|signature| !signature.is_pub)
    {
        return Err(PackageError::Manifest(format!(
            "persona '{name}' entry_workflow '{entry_workflow}' is not exported by the resolved module"
        )));
    }
    Ok(harn_vm::VmCallable::Pipeline(
        harn_vm::LazyPipelineCallable::new(module_path, function_name),
    ))
}

fn persona_stage_decl_to_runtime(stage: &PersonaStageDecl) -> harn_vm::StageDecl {
    harn_vm::StageDecl {
        name: stage.name.clone(),
        allowed_tools: stage.allowed_tools.clone(),
        side_effect_level: stage.side_effect_level.clone(),
        max_iterations: stage.max_iterations,
        on_exit: stage.on_exit.as_ref().map(|exit| harn_vm::StageExit {
            on_complete: exit.on_complete.clone(),
            on_failure: exit.on_failure.clone(),
            policy_override: None,
        }),
    }
}

fn persona_template_ref(persona: &PersonaManifestEntry) -> Option<String> {
    persona
        .package_source
        .package
        .as_ref()
        .zip(persona.version.as_ref())
        .map(|(package, version)| format!("{package}@{version}"))
        .or_else(|| persona.package_source.package.clone())
        .or_else(|| {
            persona
                .name
                .as_ref()
                .zip(persona.version.as_ref())
                .map(|(name, version)| format!("{name}@{version}"))
        })
}
