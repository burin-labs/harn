use super::*;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRuntimePersona {
    pub id: String,
    pub persona: PersonaManifestEntry,
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub execution_guard: Option<Arc<harn_modules::package_execution::PackageExecutionGuard>>,
    capabilities_materialized: bool,
}

pub(crate) fn resolve_runtime_personas(
    manifest: Manifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
    package_snapshot: Option<Arc<harn_modules::package_snapshot::PackageSnapshot>>,
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
        .map(|persona| ResolvedRuntimePersona {
            id: persona
                .name
                .clone()
                .expect("validated persona has a required name"),
            persona: persona.clone(),
            manifest_path: root.manifest_path.clone(),
            manifest_dir: root.manifest_dir.clone(),
            execution_guard: None,
            capabilities_materialized: false,
        })
        .collect::<Vec<_>>();

    if root.manifest_path.file_name() == Some(OsStr::new(MANIFEST)) {
        let ledger = load_activation_ledger(&root.manifest_dir)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        for activation in ledger.activations.into_values() {
            let discovered = resolve_discoverable_persona_in_root_with_snapshot(
                &root,
                &activation.persona_id,
                package_snapshot.as_deref(),
            )
            .map_err(PackageError::Manifest)?;
            let persona = materialize_activated_persona(&discovered, &activation)
                .map_err(|error| PackageError::Manifest(error.to_string()))?;
            let snapshot = package_snapshot.as_ref().ok_or_else(|| {
                PackageError::Manifest(format!(
                    "activated persona '{}' has no installed package generation",
                    activation.persona_id
                ))
            })?;
            let execution_guard =
                harn_modules::package_execution::PackageExecutionGuard::new_with_lock_digest(
                    Arc::clone(snapshot),
                    activation.package.alias.clone(),
                    activation.package.content_hash.clone(),
                    activation.package.lock_digest.clone(),
                )
                .map(Arc::new)
                .map_err(|error| PackageError::Manifest(error.to_string()))?;
            resolved.push(ResolvedRuntimePersona {
                id: discovered.id,
                persona,
                manifest_path: discovered.manifest_path,
                manifest_dir: discovered.manifest_dir,
                execution_guard: Some(execution_guard),
                capabilities_materialized: true,
            });
        }
    }
    resolved.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(resolved)
}

/// Project an installed package's canonical root triggers only after their
/// target persona has crossed the activation-ledger boundary.
pub(crate) fn installed_persona_trigger_configs(
    personas: &[ResolvedRuntimePersona],
) -> Result<Vec<ResolvedTriggerConfig>, PackageError> {
    let mut triggers = Vec::new();
    for resolved in personas {
        let Some(guard) = &resolved.execution_guard else {
            continue;
        };
        let Some((package_alias, persona_name)) = resolved.id.split_once('/') else {
            return Err(PackageError::Manifest(format!(
                "activated installed persona '{}' must use <package>/<persona> identity",
                resolved.id
            )));
        };
        let bytes = guard
            .verify_entry_source(&resolved.manifest_path)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            PackageError::Manifest(format!(
                "activated package manifest {} is not valid UTF-8: {error}",
                resolved.manifest_path.display()
            ))
        })?;
        let manifest = toml::from_str::<Manifest>(source).map_err(|error| {
            PackageError::Manifest(format!(
                "failed to parse activated package manifest {}: {error}",
                resolved.manifest_path.display()
            ))
        })?;
        let unqualified_handler = format!("persona://{persona_name}");
        let qualified_handler = format!("persona://{}", resolved.id);
        for mut trigger in resolved_triggers_from_manifest(&manifest, &resolved.manifest_dir) {
            let handler = trigger.handler.trim();
            if handler != unqualified_handler && handler != qualified_handler {
                continue;
            }
            trigger.id = format!("{package_alias}/{}", trigger.id);
            trigger.handler.clone_from(&qualified_handler);
            triggers.push(trigger);
        }
    }
    Ok(triggers)
}

pub(crate) fn persona_runtime_handler_for_trigger(
    extensions: &RuntimeExtensions,
    trigger: &ResolvedTriggerConfig,
    name: &str,
) -> Result<
    (
        harn_vm::PersonaRuntimeBinding,
        harn_vm::VmCallable,
        harn_vm::AutonomyTier,
    ),
    PackageError,
> {
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
    let callable = persona_runtime_callable_with_guard(
        name,
        &resolved.persona,
        &resolved.manifest_dir,
        resolved.execution_guard.clone(),
        resolved.capabilities_materialized,
    )
    .map_err(|error| trigger_error(trigger, error.to_string()))?;
    Ok((
        persona_runtime_binding(name, &resolved.persona),
        callable,
        persona_autonomy_ceiling(&resolved.persona),
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
    let runtime_binding = persona_runtime_binding(&resolved.id, &resolved.persona);
    let callable = persona_runtime_callable_with_guard(
        &resolved.id,
        &resolved.persona,
        &resolved.manifest_dir,
        resolved.execution_guard.clone(),
        resolved.capabilities_materialized,
    )?;
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
        autonomy_tier: persona_autonomy_ceiling(&resolved.persona),
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
    harn_vm::PersonaRuntimeBinding {
        name: name.to_string(),
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
) -> Result<harn_vm::orchestration::CapabilityPolicy, PackageError> {
    capability_policy(&persona.tools, &normalized_persona_capabilities(persona))
}

fn capability_policy(
    tools: &[String],
    persona_capabilities: &[String],
) -> Result<harn_vm::orchestration::CapabilityPolicy, PackageError> {
    let mut capabilities = BTreeMap::new();
    for capability in persona_capabilities {
        let (name, operation) = capability.split_once('.').ok_or_else(|| {
            PackageError::Manifest(format!(
                "persona capability '{capability}' must have capability.operation form"
            ))
        })?;
        capabilities
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(operation.to_string());
    }
    let mut policy = harn_vm::orchestration::CapabilityPolicy::default();
    policy.restrict_tools(tools.to_vec());
    policy.restrict_capabilities(capabilities);
    Ok(policy)
}

pub(crate) fn persona_autonomy_to_vm(value: PersonaAutonomyTier) -> harn_vm::AutonomyTier {
    match value {
        PersonaAutonomyTier::Shadow => harn_vm::AutonomyTier::Shadow,
        PersonaAutonomyTier::Suggest => harn_vm::AutonomyTier::Suggest,
        PersonaAutonomyTier::ActWithApproval => harn_vm::AutonomyTier::ActWithApproval,
        PersonaAutonomyTier::ActAuto => harn_vm::AutonomyTier::ActAuto,
    }
}

fn persona_autonomy_ceiling(persona: &PersonaManifestEntry) -> harn_vm::AutonomyTier {
    persona
        .autonomy_tier
        .map(persona_autonomy_to_vm)
        .unwrap_or(harn_vm::AutonomyTier::Suggest)
}

pub(crate) fn persona_runtime_callable(
    name: &str,
    persona: &PersonaManifestEntry,
    manifest_dir: &Path,
) -> Result<harn_vm::VmCallable, PackageError> {
    persona_runtime_callable_with_guard(name, persona, manifest_dir, None, false)
}

fn persona_runtime_callable_with_guard(
    name: &str,
    persona: &PersonaManifestEntry,
    manifest_dir: &Path,
    execution_guard: Option<Arc<harn_modules::package_execution::PackageExecutionGuard>>,
    capabilities_materialized: bool,
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
    let execution_policy = if capabilities_materialized {
        capability_policy(&persona.tools, &persona.capabilities)?
    } else {
        persona_execution_policy(persona)?
    };
    let mut callable = harn_vm::LazyPipelineCallable::new(module_path, function_name)
        .with_execution_policy(execution_policy)
        .with_autonomy_ceiling(persona_autonomy_ceiling(persona));
    if let Some(execution_guard) = execution_guard {
        callable = callable.with_package_execution_guard(execution_guard);
    }
    Ok(harn_vm::VmCallable::Pipeline(callable))
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
