use super::*;

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
