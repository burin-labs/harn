use super::*;

pub(crate) fn manifest_capabilities(
    manifest: &Manifest,
) -> Option<&harn_vm::llm::capabilities::CapabilitiesFile> {
    manifest.capabilities.as_ref()
}

pub(crate) fn is_empty_capabilities(file: &harn_vm::llm::capabilities::CapabilitiesFile) -> bool {
    file.provider.is_empty() && file.provider_family.is_empty()
}

/// Load the nearest project manifest plus any installed package manifests and
/// merge the root project's runtime extensions.
pub fn try_load_runtime_extensions(anchor: &Path) -> Result<RuntimeExtensions, String> {
    ensure_dependencies_materialized(anchor)?;
    let Some((root_manifest, manifest_dir)) = find_nearest_manifest(anchor) else {
        return Ok(RuntimeExtensions::default());
    };

    let mut llm = harn_vm::llm_config::ProvidersConfig::default();
    let mut capabilities = harn_vm::llm::capabilities::CapabilitiesFile::default();
    let mut hooks = Vec::new();
    let mut triggers = Vec::new();

    llm.merge_from(&root_manifest.llm);
    if let Some(file) = manifest_capabilities(&root_manifest) {
        merge_capability_overrides(&mut capabilities, file);
    }
    hooks.extend(resolved_hooks_from_manifest(&root_manifest, &manifest_dir));
    triggers.extend(resolved_triggers_from_manifest(
        &root_manifest,
        &manifest_dir,
    ));
    let provider_connectors =
        resolved_provider_connectors_from_manifest(&root_manifest, &manifest_dir);

    Ok(RuntimeExtensions {
        root_manifest_path: Some(manifest_dir.join(MANIFEST)),
        root_manifest_dir: Some(manifest_dir),
        root_manifest: Some(root_manifest),
        llm: (!llm.is_empty()).then_some(llm),
        capabilities: (!is_empty_capabilities(&capabilities)).then_some(capabilities),
        hooks,
        triggers,
        provider_connectors,
    })
}

pub fn load_runtime_extensions(anchor: &Path) -> RuntimeExtensions {
    match try_load_runtime_extensions(anchor) {
        Ok(extensions) => extensions,
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

/// Install merged runtime extensions on the current thread.
pub fn install_runtime_extensions(extensions: &RuntimeExtensions) {
    harn_vm::llm_config::set_user_overrides(extensions.llm.clone());
    harn_vm::llm::capabilities::set_user_overrides(extensions.capabilities.clone());
    install_orchestrator_budget(extensions);
}

pub fn install_orchestrator_budget(extensions: &RuntimeExtensions) {
    let budget = extensions
        .root_manifest
        .as_ref()
        .map(|manifest| harn_vm::OrchestratorBudgetConfig {
            daily_cost_usd: manifest.orchestrator.budget.daily_cost_usd,
            hourly_cost_usd: manifest.orchestrator.budget.hourly_cost_usd,
        })
        .unwrap_or_default();
    harn_vm::install_orchestrator_budget(budget);
}

pub async fn install_manifest_hooks(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    harn_vm::orchestration::clear_runtime_hooks();
    let mut loaded_exports: HashMap<ManifestModuleCacheKey, ManifestModuleExports> = HashMap::new();
    for hook in &extensions.hooks {
        let Some((module_name, function_name)) = hook.handler.rsplit_once("::") else {
            return Err(format!(
                "invalid hook handler '{}': expected <module>::<function>",
                hook.handler
            ));
        };
        let cache_key = (
            hook.manifest_dir.clone(),
            hook.package_name.clone(),
            Some(module_name.to_string()),
        );
        if !loaded_exports.contains_key(&cache_key) {
            let exports = resolve_manifest_exports(
                vm,
                &hook.manifest_dir,
                hook.package_name.as_deref(),
                &hook.exports,
                Some(module_name),
            )
            .await?;
            loaded_exports.insert(cache_key.clone(), exports);
        }
        let exports = loaded_exports
            .get(&cache_key)
            .expect("manifest hook exports cached");
        let Some(closure) = exports.get(function_name) else {
            return Err(format!(
                "hook handler '{}' is not exported by module '{}'",
                function_name, module_name
            ));
        };
        harn_vm::orchestration::register_vm_hook(
            hook.event,
            hook.pattern.clone(),
            hook.handler.clone(),
            closure.clone(),
        );
    }
    Ok(())
}

pub async fn collect_manifest_triggers(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<Vec<CollectedManifestTrigger>, String> {
    let _provider_schema_guard = lock_manifest_provider_schemas().await;
    install_manifest_provider_schemas(extensions).await?;
    validate_orchestrator_budget(extensions.root_manifest.as_ref())?;
    validate_static_trigger_configs(&extensions.triggers)?;
    let mut loaded_exports: HashMap<ManifestModuleCacheKey, ManifestModuleExports> = HashMap::new();
    let mut module_signatures: HashMap<PathBuf, BTreeMap<String, TriggerFunctionSignature>> =
        HashMap::new();
    let mut collected = Vec::new();

    for trigger in &extensions.triggers {
        let handler = parse_trigger_handler_uri(trigger)?;
        let collected_handler = match handler {
            TriggerHandlerUri::Local(reference) => {
                let cache_key = (
                    trigger.manifest_dir.clone(),
                    trigger.package_name.clone(),
                    reference.module_name.clone(),
                );
                if !loaded_exports.contains_key(&cache_key) {
                    let exports = resolve_manifest_exports(
                        vm,
                        &trigger.manifest_dir,
                        trigger.package_name.as_deref(),
                        &trigger.exports,
                        reference.module_name.as_deref(),
                    )
                    .await
                    .map_err(|error| trigger_error(trigger, error))?;
                    loaded_exports.insert(cache_key.clone(), exports);
                }
                let exports = loaded_exports
                    .get(&cache_key)
                    .expect("manifest trigger exports cached");
                let Some(closure) = exports.get(&reference.function_name) else {
                    return Err(trigger_error(
                        trigger,
                        format!(
                            "handler '{}' is not exported by the resolved module",
                            reference.raw
                        ),
                    ));
                };
                CollectedTriggerHandler::Local {
                    reference,
                    closure: closure.clone(),
                }
            }
            TriggerHandlerUri::A2a {
                target,
                allow_cleartext,
            } => CollectedTriggerHandler::A2a {
                target,
                allow_cleartext,
            },
            TriggerHandlerUri::Worker { queue } => CollectedTriggerHandler::Worker { queue },
            TriggerHandlerUri::Persona { name } => {
                let binding = persona_runtime_binding_for_handler(extensions, trigger, &name)?;
                CollectedTriggerHandler::Persona { binding }
            }
        };

        let collected_when = if let Some(when_raw) = &trigger.when {
            let reference = parse_local_trigger_ref(when_raw, "when", trigger)?;
            let cache_key = (
                trigger.manifest_dir.clone(),
                trigger.package_name.clone(),
                reference.module_name.clone(),
            );
            if !loaded_exports.contains_key(&cache_key) {
                let exports = resolve_manifest_exports(
                    vm,
                    &trigger.manifest_dir,
                    trigger.package_name.as_deref(),
                    &trigger.exports,
                    reference.module_name.as_deref(),
                )
                .await
                .map_err(|error| trigger_error(trigger, error))?;
                loaded_exports.insert(cache_key.clone(), exports);
            }
            let exports = loaded_exports
                .get(&cache_key)
                .expect("manifest trigger predicate exports cached");
            let Some(closure) = exports.get(&reference.function_name) else {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' is not exported by the resolved module",
                        reference.raw
                    ),
                ));
            };

            let source_path = manifest_module_source_path(
                &trigger.manifest_dir,
                trigger.package_name.as_deref(),
                &trigger.exports,
                reference.module_name.as_deref(),
            )
            .map_err(|error| trigger_error(trigger, error))?;
            if !module_signatures.contains_key(&source_path) {
                let signatures = load_trigger_function_signatures(&source_path)
                    .map_err(|error| trigger_error(trigger, error))?;
                module_signatures.insert(source_path.clone(), signatures);
            }
            let signatures = module_signatures
                .get(&source_path)
                .expect("module signatures cached");
            let Some(signature) = signatures.get(&reference.function_name) else {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must resolve to a function declaration",
                        reference.raw
                    ),
                ));
            };
            if signature.params.len() != 1
                || signature.params[0]
                    .as_ref()
                    .is_none_or(|param| !is_trigger_event_type(param))
            {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must have signature fn(TriggerEvent) -> bool",
                        reference.raw
                    ),
                ));
            }
            if signature
                .return_type
                .as_ref()
                .is_none_or(|return_type| !is_predicate_return_type(return_type))
            {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "when predicate '{}' must have signature fn(TriggerEvent) -> bool or Result<bool, _>",
                        reference.raw
                    ),
                ));
            }

            Some(CollectedTriggerPredicate {
                reference,
                closure: closure.clone(),
            })
        } else {
            None
        };

        let flow_control = collect_trigger_flow_control(vm, trigger).await?;

        collected.push(CollectedManifestTrigger {
            config: trigger.clone(),
            handler: collected_handler,
            when: collected_when,
            flow_control,
        });
    }

    Ok(collected)
}

pub(crate) async fn collect_trigger_flow_control(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
) -> Result<harn_vm::TriggerFlowControlConfig, String> {
    let mut flow = harn_vm::TriggerFlowControlConfig::default();

    let concurrency = if let Some(spec) = &trigger.concurrency {
        Some(spec.clone())
    } else if let Some(max) = trigger.budget.max_concurrent {
        eprintln!(
            "warning: {} uses deprecated budget.max_concurrent; prefer concurrency = {{ max = {} }}",
            manifest_trigger_location(trigger),
            max
        );
        Some(TriggerConcurrencyManifestSpec { key: None, max })
    } else {
        None
    };
    if let Some(spec) = concurrency {
        flow.concurrency = Some(harn_vm::TriggerConcurrencyConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "concurrency.key",
                spec.key.as_deref(),
            )
            .await?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.throttle {
        flow.throttle = Some(harn_vm::TriggerThrottleConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "throttle.key",
                spec.key.as_deref(),
            )
            .await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("throttle.period {error}")))?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.rate_limit {
        flow.rate_limit = Some(harn_vm::TriggerRateLimitConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "rate_limit.key",
                spec.key.as_deref(),
            )
            .await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("rate_limit.period {error}")))?,
            max: spec.max,
        });
    }

    if let Some(spec) = &trigger.debounce {
        flow.debounce = Some(harn_vm::TriggerDebounceConfig {
            key: compile_trigger_expression(vm, trigger, "debounce.key", &spec.key).await?,
            period: harn_vm::parse_flow_control_duration(&spec.period)
                .map_err(|error| trigger_error(trigger, format!("debounce.period {error}")))?,
        });
    }

    if let Some(spec) = &trigger.singleton {
        flow.singleton = Some(harn_vm::TriggerSingletonConfig {
            key: compile_optional_trigger_expression(
                vm,
                trigger,
                "singleton.key",
                spec.key.as_deref(),
            )
            .await?,
        });
    }

    if let Some(spec) = &trigger.batch {
        flow.batch = Some(harn_vm::TriggerBatchConfig {
            key: compile_optional_trigger_expression(vm, trigger, "batch.key", spec.key.as_deref())
                .await?,
            size: spec.size,
            timeout: harn_vm::parse_flow_control_duration(&spec.timeout)
                .map_err(|error| trigger_error(trigger, format!("batch.timeout {error}")))?,
        });
    }

    if let Some(spec) = &trigger.priority_flow {
        flow.priority = Some(harn_vm::TriggerPriorityOrderConfig {
            key: compile_trigger_expression(vm, trigger, "priority.key", &spec.key).await?,
            order: spec.order.clone(),
        });
    }

    Ok(flow)
}

fn persona_runtime_binding_for_handler(
    extensions: &RuntimeExtensions,
    trigger: &ResolvedTriggerConfig,
    name: &str,
) -> Result<harn_vm::PersonaRuntimeBinding, String> {
    let Some(manifest) = extensions.root_manifest.as_ref() else {
        return Err(trigger_error(
            trigger,
            format!("handler persona://{name} requires a root manifest"),
        ));
    };
    let Some(persona) = manifest
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(name))
    else {
        return Err(trigger_error(
            trigger,
            format!("handler persona://{name} does not match a declared persona"),
        ));
    };
    Ok(harn_vm::PersonaRuntimeBinding {
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
    })
}

pub(crate) async fn compile_optional_trigger_expression(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: Option<&str>,
) -> Result<Option<harn_vm::TriggerExpressionSpec>, String> {
    match expr {
        Some(expr) => compile_trigger_expression(vm, trigger, field_name, expr)
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub(crate) async fn compile_trigger_expression(
    vm: &mut harn_vm::Vm,
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: &str,
) -> Result<harn_vm::TriggerExpressionSpec, String> {
    let synthetic = PathBuf::from(format!(
        "<trigger-expr>/{}/{:04}-{}.harn",
        harn_vm::event_log::sanitize_topic_component(&trigger.id),
        trigger.table_index,
        harn_vm::event_log::sanitize_topic_component(field_name),
    ));
    let source = format!(
        "import \"std/triggers\"\n\npub fn __trigger_expr(event: TriggerEvent) -> any {{\n  return {expr}\n}}\n"
    );
    let exports = vm
        .load_module_exports_from_source(synthetic, &source)
        .await
        .map_err(|error| {
            trigger_error(
                trigger,
                format!("{field_name} '{expr}' is invalid Harn expression: {error}"),
            )
        })?;
    let closure = exports.get("__trigger_expr").ok_or_else(|| {
        trigger_error(
            trigger,
            format!("{field_name} '{expr}' did not compile into an exported closure"),
        )
    })?;
    Ok(harn_vm::TriggerExpressionSpec {
        raw: expr.to_string(),
        closure: closure.clone(),
    })
}

pub(crate) fn trigger_kind_label(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Webhook => "webhook",
        TriggerKind::Cron => "cron",
        TriggerKind::Poll => "poll",
        TriggerKind::Stream => "stream",
        TriggerKind::Predicate => "predicate",
        TriggerKind::A2aPush => "a2a-push",
    }
}

pub(crate) fn worker_queue_priority(
    priority: TriggerDispatchPriority,
) -> harn_vm::WorkerQueuePriority {
    match priority {
        TriggerDispatchPriority::High => harn_vm::WorkerQueuePriority::High,
        TriggerDispatchPriority::Normal => harn_vm::WorkerQueuePriority::Normal,
        TriggerDispatchPriority::Low => harn_vm::WorkerQueuePriority::Low,
    }
}

pub fn manifest_trigger_binding_spec(
    trigger: CollectedManifestTrigger,
) -> harn_vm::TriggerBindingSpec {
    let flow_control = trigger.flow_control.clone();
    let config = trigger.config;
    let (handler, handler_descriptor) = match trigger.handler {
        CollectedTriggerHandler::Local { reference, closure } => (
            harn_vm::TriggerHandlerSpec::Local {
                raw: reference.raw.clone(),
                closure,
            },
            serde_json::json!({
                "kind": "local",
                "raw": reference.raw,
            }),
        ),
        CollectedTriggerHandler::A2a {
            target,
            allow_cleartext,
        } => (
            harn_vm::TriggerHandlerSpec::A2a {
                target: target.clone(),
                allow_cleartext,
            },
            serde_json::json!({
                "kind": "a2a",
                "target": target,
                "allow_cleartext": allow_cleartext,
            }),
        ),
        CollectedTriggerHandler::Worker { queue } => (
            harn_vm::TriggerHandlerSpec::Worker {
                queue: queue.clone(),
            },
            serde_json::json!({
                "kind": "worker",
                "queue": queue,
            }),
        ),
        CollectedTriggerHandler::Persona { binding } => (
            harn_vm::TriggerHandlerSpec::Persona {
                binding: binding.clone(),
            },
            serde_json::json!({
                "kind": "persona",
                "name": binding.name,
                "entry_workflow": binding.entry_workflow,
            }),
        ),
    };

    let when_raw = trigger
        .when
        .as_ref()
        .map(|predicate| predicate.reference.raw.clone());
    let when = trigger.when.map(|predicate| harn_vm::TriggerPredicateSpec {
        raw: predicate.reference.raw,
        closure: predicate.closure,
    });
    let mut when_budget = config
        .when_budget
        .as_ref()
        .map(|budget| {
            Ok::<harn_vm::TriggerPredicateBudget, String>(harn_vm::TriggerPredicateBudget {
                max_cost_usd: budget.max_cost_usd,
                tokens_max: budget.tokens_max,
                timeout_ms: budget
                    .timeout
                    .as_deref()
                    .map(parse_duration_millis)
                    .transpose()?,
            })
        })
        .transpose()
        .unwrap_or_default();
    if config.budget.max_cost_usd.is_some() || config.budget.max_tokens.is_some() {
        let budget = when_budget.get_or_insert_with(harn_vm::TriggerPredicateBudget::default);
        if budget.max_cost_usd.is_none() {
            budget.max_cost_usd = config.budget.max_cost_usd;
        }
        if budget.tokens_max.is_none() {
            budget.tokens_max = config.budget.max_tokens;
        }
    }
    let id = config.id.clone();
    let kind = trigger_kind_label(config.kind).to_string();
    let provider = config.provider.clone();
    let autonomy_tier = config.autonomy_tier;
    let match_events = config.match_.events.clone();
    let dedupe_key = config.dedupe_key.clone();
    let retry = harn_vm::TriggerRetryConfig::new(
        config.retry.max,
        match config.retry.backoff {
            TriggerRetryBackoff::Immediate => harn_vm::RetryPolicy::Linear { delay_ms: 0 },
            TriggerRetryBackoff::Svix => harn_vm::RetryPolicy::Svix,
        },
    );
    let filter = config.filter.clone();
    let dedupe_retention_days = config.retry.retention_days;
    let daily_cost_usd = config.budget.daily_cost_usd;
    let hourly_cost_usd = config.budget.hourly_cost_usd;
    let max_autonomous_decisions_per_hour = config.budget.max_autonomous_decisions_per_hour;
    let max_autonomous_decisions_per_day = config.budget.max_autonomous_decisions_per_day;
    let on_budget_exhausted = config.budget.on_budget_exhausted;
    let max_concurrent = flow_control.concurrency.as_ref().map(|config| config.max);
    let manifest_path = Some(config.manifest_path.clone());
    let package_name = config.package_name.clone();

    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": &id,
        "kind": &kind,
        "provider": provider.as_str(),
        "autonomy_tier": autonomy_tier,
        "match": config.match_,
        "when": when_raw,
        "when_budget": config.when_budget,
        "handler": handler_descriptor,
        "dedupe_key": &dedupe_key,
        "retry": config.retry,
        "dispatch_priority": config.dispatch_priority,
        "budget": config.budget,
        "flow_control": {
            "concurrency": config.concurrency,
            "throttle": config.throttle,
            "rate_limit": config.rate_limit,
            "debounce": config.debounce,
            "singleton": config.singleton,
            "batch": config.batch,
            "priority": config.priority_flow,
        },
        "window": config.window,
        "secrets": config.secrets,
        "filter": &filter,
        "kind_specific": config.kind_specific,
        "manifest_path": &manifest_path,
        "package_name": &package_name,
    }))
    .unwrap_or_else(|_| format!("{}:{}:{}", id, kind, provider.as_str()));

    harn_vm::TriggerBindingSpec {
        id,
        source: harn_vm::TriggerBindingSource::Manifest,
        kind,
        provider,
        autonomy_tier,
        handler,
        dispatch_priority: worker_queue_priority(config.dispatch_priority),
        when,
        when_budget,
        retry,
        match_events,
        dedupe_key,
        filter,
        dedupe_retention_days,
        daily_cost_usd,
        hourly_cost_usd,
        max_autonomous_decisions_per_hour,
        max_autonomous_decisions_per_day,
        on_budget_exhausted,
        max_concurrent,
        flow_control,
        manifest_path,
        package_name,
        definition_fingerprint: fingerprint,
    }
}

pub async fn install_manifest_triggers(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    install_orchestrator_budget(extensions);
    let collected = collect_manifest_triggers(vm, extensions).await?;
    let mut bindings: Vec<_> = collected
        .iter()
        .cloned()
        .map(manifest_trigger_binding_spec)
        .collect();
    bindings.extend(collect_persona_trigger_binding_specs(extensions)?);
    harn_vm::install_manifest_triggers(bindings)
        .await
        .map_err(|error| error.to_string())
}

pub async fn install_collected_manifest_triggers(
    collected: &[CollectedManifestTrigger],
) -> Result<(), String> {
    let bindings = collected
        .iter()
        .cloned()
        .map(manifest_trigger_binding_spec)
        .collect();
    harn_vm::install_manifest_triggers(bindings)
        .await
        .map_err(|error| error.to_string())
}

pub fn collect_persona_trigger_binding_specs(
    extensions: &RuntimeExtensions,
) -> Result<Vec<harn_vm::TriggerBindingSpec>, String> {
    let Some(manifest) = extensions.root_manifest.clone() else {
        return Ok(Vec::new());
    };
    let manifest_path = extensions
        .root_manifest_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(MANIFEST));
    let manifest_dir = extensions
        .root_manifest_dir
        .clone()
        .or_else(|| manifest_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let resolved = validate_and_resolve_personas(manifest, manifest_path.clone(), manifest_dir)
        .map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let mut bindings = Vec::new();
    for persona in resolved.personas {
        let Some(name) = persona.name.clone() else {
            continue;
        };
        for trigger in &persona.triggers {
            let Some((provider, kind)) = trigger.split_once('.') else {
                continue;
            };
            let provider = provider.trim();
            let kind = kind.trim();
            if provider.is_empty() || kind.is_empty() {
                continue;
            }
            bindings.push(persona_trigger_binding_spec(
                &resolved.manifest_path,
                &name,
                provider,
                kind,
                &persona,
            ));
        }
    }
    Ok(bindings)
}

fn persona_trigger_binding_spec(
    manifest_path: &Path,
    name: &str,
    provider: &str,
    kind: &str,
    persona: &PersonaManifestEntry,
) -> harn_vm::TriggerBindingSpec {
    let runtime_binding = harn_vm::PersonaRuntimeBinding {
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
    };
    let id = format!("persona.{name}.{provider}.{kind}");
    let handler = harn_vm::TriggerHandlerSpec::Persona {
        binding: runtime_binding.clone(),
    };
    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": &id,
        "kind": kind,
        "provider": provider,
        "handler": {
            "kind": "persona",
            "name": name,
            "entry_workflow": runtime_binding.entry_workflow,
        },
        "budget": runtime_binding.budget,
        "manifest_path": manifest_path,
    }))
    .unwrap_or_else(|_| format!("{id}:{provider}:{kind}:{name}"));

    harn_vm::TriggerBindingSpec {
        id,
        source: harn_vm::TriggerBindingSource::Manifest,
        kind: kind.to_string(),
        provider: harn_vm::ProviderId::from(provider.to_string()),
        autonomy_tier: persona
            .autonomy_tier
            .map(persona_autonomy_to_vm)
            .unwrap_or(harn_vm::AutonomyTier::Suggest),
        handler,
        dispatch_priority: harn_vm::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: harn_vm::TriggerRetryConfig::default(),
        match_events: vec![kind.to_string()],
        dedupe_key: None,
        filter: None,
        dedupe_retention_days: 7,
        daily_cost_usd: persona.budget.daily_usd,
        hourly_cost_usd: persona.budget.hourly_usd,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::RetryLater,
        max_concurrent: None,
        flow_control: harn_vm::TriggerFlowControlConfig::default(),
        manifest_path: Some(manifest_path.to_path_buf()),
        package_name: None,
        definition_fingerprint: fingerprint,
    }
}

fn persona_autonomy_to_vm(value: PersonaAutonomyTier) -> harn_vm::AutonomyTier {
    match value {
        PersonaAutonomyTier::Shadow => harn_vm::AutonomyTier::Shadow,
        PersonaAutonomyTier::Suggest => harn_vm::AutonomyTier::Suggest,
        PersonaAutonomyTier::ActWithApproval => harn_vm::AutonomyTier::ActWithApproval,
        PersonaAutonomyTier::ActAuto => harn_vm::AutonomyTier::ActAuto,
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

pub fn load_personas_from_manifest_path(
    manifest_path: &Path,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let manifest_path = if manifest_path.is_dir() {
        manifest_path.join(MANIFEST)
    } else {
        manifest_path.to_path_buf()
    };
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let manifest = match read_manifest_from_path(&manifest_path) {
        Ok(manifest) => manifest,
        Err(message) => {
            if let Ok(document) =
                harn_modules::personas::parse_persona_manifest_file(&manifest_path)
            {
                if !document.personas.is_empty() {
                    return validate_and_resolve_standalone_personas(
                        document.personas,
                        manifest_path,
                        manifest_dir,
                    );
                }
            }
            return Err(vec![PersonaValidationError {
                manifest_path: manifest_path.clone(),
                field_path: "harn.toml".to_string(),
                message,
            }]);
        }
    };
    if manifest.personas.is_empty() {
        if let Ok(document) = harn_modules::personas::parse_persona_manifest_file(&manifest_path) {
            if !document.personas.is_empty() {
                return validate_and_resolve_standalone_personas(
                    document.personas,
                    manifest_path,
                    manifest_dir,
                );
            }
        }
    }
    validate_and_resolve_personas(manifest, manifest_path, manifest_dir)
}

fn validate_and_resolve_standalone_personas(
    personas: Vec<PersonaManifestEntry>,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let known_names = personas
        .iter()
        .filter_map(|persona| persona.name.as_ref())
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    let context = harn_modules::personas::PersonaValidationContext {
        known_capabilities: harn_modules::personas::default_persona_capabilities(),
        known_tools: BTreeSet::new(),
        known_names,
    };
    harn_modules::personas::validate_persona_manifests(&manifest_path, &personas, &context)?;
    Ok(ResolvedPersonaManifest {
        manifest_path,
        manifest_dir,
        personas,
    })
}

pub fn load_personas_config(
    anchor: Option<&Path>,
) -> Result<Option<ResolvedPersonaManifest>, Vec<PersonaValidationError>> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let Some((manifest, dir)) = find_nearest_manifest(&anchor) else {
        return Ok(None);
    };
    let manifest_path = dir.join(MANIFEST);
    validate_and_resolve_personas(manifest, manifest_path, dir).map(Some)
}

pub(crate) fn validate_and_resolve_personas(
    manifest: Manifest,
    manifest_path: PathBuf,
    manifest_dir: PathBuf,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let known_capabilities = known_persona_capabilities(&manifest, &manifest_dir);
    let known_tools = known_persona_tools(&manifest);
    let known_names: BTreeSet<String> = manifest
        .personas
        .iter()
        .filter_map(|persona| persona.name.as_ref())
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    let context = harn_modules::personas::PersonaValidationContext {
        known_capabilities,
        known_tools,
        known_names,
    };
    if let Err(errors) = harn_modules::personas::validate_persona_manifests(
        &manifest_path,
        &manifest.personas,
        &context,
    ) {
        Err(errors)
    } else {
        Ok(ResolvedPersonaManifest {
            manifest_path,
            manifest_dir,
            personas: manifest.personas,
        })
    }
}

pub(crate) fn known_persona_capabilities(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> BTreeSet<String> {
    let mut capabilities = BTreeSet::new();
    for (capability, operations) in default_persona_capability_map() {
        for operation in operations {
            capabilities.insert(format!("{capability}.{operation}"));
        }
    }
    for (capability, operations) in &manifest.check.host_capabilities {
        for operation in operations {
            capabilities.insert(format!("{capability}.{operation}"));
        }
    }
    if let Some(path) = manifest.check.host_capabilities_path.as_deref() {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            manifest_dir.join(path)
        };
        if let Ok(content) = fs::read_to_string(path) {
            let parsed_json = serde_json::from_str::<serde_json::Value>(&content).ok();
            let parsed_toml = toml::from_str::<toml::Value>(&content)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok());
            if let Some(value) = parsed_json.or(parsed_toml) {
                collect_persona_capabilities_from_json(&value, &mut capabilities);
            }
        }
    }
    capabilities
}

pub(crate) fn collect_persona_capabilities_from_json(
    value: &serde_json::Value,
    out: &mut BTreeSet<String>,
) {
    let root = value.get("capabilities").unwrap_or(value);
    let Some(capabilities) = root.as_object() else {
        return;
    };
    for (capability, entry) in capabilities {
        if let Some(list) = entry.as_array() {
            for item in list {
                if let Some(operation) = item.as_str() {
                    out.insert(format!("{capability}.{operation}"));
                }
            }
        } else if let Some(obj) = entry.as_object() {
            if let Some(list) = obj
                .get("operations")
                .or_else(|| obj.get("ops"))
                .and_then(|v| v.as_array())
            {
                for item in list {
                    if let Some(operation) = item.as_str() {
                        out.insert(format!("{capability}.{operation}"));
                    }
                }
            } else {
                for (operation, enabled) in obj {
                    if enabled.as_bool().unwrap_or(true) {
                        out.insert(format!("{capability}.{operation}"));
                    }
                }
            }
        }
    }
}

pub(crate) fn default_persona_capability_map() -> BTreeMap<&'static str, Vec<&'static str>> {
    harn_modules::personas::default_persona_capability_map()
}

pub(crate) fn known_persona_tools(manifest: &Manifest) -> BTreeSet<String> {
    let mut tools = BTreeSet::from([
        "a2a".to_string(),
        "acp".to_string(),
        "ci".to_string(),
        "filesystem".to_string(),
        "github".to_string(),
        "linear".to_string(),
        "mcp".to_string(),
        "notion".to_string(),
        "pagerduty".to_string(),
        "shell".to_string(),
        "slack".to_string(),
    ]);
    for server in &manifest.mcp {
        tools.insert(server.name.clone());
    }
    for provider in &manifest.providers {
        tools.insert(provider.id.as_str().to_string());
    }
    for trigger in &manifest.triggers {
        if let Some(provider) = trigger.provider.as_ref() {
            tools.insert(provider.as_str().to_string());
        }
        for source in &trigger.sources {
            tools.insert(source.provider.as_str().to_string());
        }
    }
    tools
}

#[cfg(test)]
#[path = "extensions_tests.rs"]
mod tests;
