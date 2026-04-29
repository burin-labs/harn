use super::*;

pub(crate) fn merge_capability_overrides(
    target: &mut harn_vm::llm::capabilities::CapabilitiesFile,
    source: &harn_vm::llm::capabilities::CapabilitiesFile,
) {
    for (provider, rules) in &source.provider {
        target
            .provider
            .entry(provider.clone())
            .or_default()
            .extend(rules.clone());
    }
    target
        .provider_family
        .extend(source.provider_family.clone());
}

pub(crate) fn resolved_hooks_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedHookConfig> {
    manifest
        .hooks
        .iter()
        .map(|hook| ResolvedHookConfig {
            event: hook.event,
            pattern: hook.pattern.clone(),
            handler: hook.handler.clone(),
            manifest_dir: manifest_dir.to_path_buf(),
            package_name: manifest.package.as_ref().and_then(|pkg| pkg.name.clone()),
            exports: manifest.exports.clone(),
        })
        .collect()
}

pub(crate) fn resolved_triggers_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedTriggerConfig> {
    let manifest_path = manifest_dir.join(MANIFEST);
    let package_name = manifest.package.as_ref().and_then(|pkg| pkg.name.clone());
    manifest
        .triggers
        .iter()
        .enumerate()
        .flat_map(|(table_index, trigger)| {
            resolved_trigger_entries_from_manifest_table(
                trigger,
                manifest_dir,
                &manifest_path,
                package_name.clone(),
                manifest.exports.clone(),
                table_index,
            )
        })
        .collect()
}

pub(crate) fn resolved_trigger_entries_from_manifest_table(
    trigger: &TriggerManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
) -> Vec<ResolvedTriggerConfig> {
    if trigger.sources.is_empty() {
        return vec![resolved_single_trigger_entry(
            trigger,
            manifest_dir,
            manifest_path,
            package_name,
            exports,
            table_index,
        )];
    }

    trigger
        .sources
        .iter()
        .enumerate()
        .map(|(source_index, source)| {
            resolved_trigger_source_entry(
                trigger,
                source,
                manifest_dir,
                manifest_path,
                package_name.clone(),
                exports.clone(),
                table_index,
                source_index,
            )
        })
        .collect()
}

pub(crate) fn resolved_single_trigger_entry(
    trigger: &TriggerManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
) -> ResolvedTriggerConfig {
    let shape_error = match (&trigger.kind, &trigger.provider) {
        (None, None) => {
            Some("trigger table must set kind/provider or declare one or more sources".to_string())
        }
        (None, Some(_)) => Some("trigger table missing kind".to_string()),
        (Some(_), None) => Some("trigger table missing provider".to_string()),
        (Some(_), Some(_)) => None,
    }
    .or_else(|| {
        trigger
            .match_
            .is_none()
            .then_some("trigger table missing match".to_string())
    });
    let (dispatch_priority, priority_flow) = split_trigger_priority(trigger.priority.clone());
    ResolvedTriggerConfig {
        id: trigger.id.clone(),
        kind: trigger.kind.unwrap_or(TriggerKind::Webhook),
        provider: trigger
            .provider
            .clone()
            .unwrap_or_else(|| harn_vm::ProviderId::from("")),
        autonomy_tier: trigger.autonomy_tier,
        match_: trigger.match_.clone().unwrap_or_default(),
        when: trigger.when.clone(),
        when_budget: trigger.when_budget.clone(),
        handler: trigger.handler.clone(),
        dedupe_key: trigger.dedupe_key.clone(),
        retry: trigger.retry.clone(),
        dispatch_priority,
        budget: trigger.budget.clone(),
        concurrency: trigger.concurrency.clone(),
        throttle: trigger.throttle.clone(),
        rate_limit: trigger.rate_limit.clone(),
        debounce: trigger.debounce.clone(),
        singleton: trigger.singleton.clone(),
        batch: trigger.batch.clone(),
        window: trigger.window.clone(),
        priority_flow,
        secrets: trigger.secrets.clone(),
        filter: trigger.filter.clone(),
        kind_specific: trigger.kind_specific.clone(),
        manifest_dir: manifest_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        package_name,
        exports,
        table_index,
        shape_error,
    }
}

pub(crate) fn resolved_trigger_source_entry(
    trigger: &TriggerManifestEntry,
    source: &TriggerSourceManifestEntry,
    manifest_dir: &Path,
    manifest_path: &Path,
    package_name: Option<String>,
    exports: HashMap<String, String>,
    table_index: usize,
    source_index: usize,
) -> ResolvedTriggerConfig {
    let (dispatch_priority, priority_flow) =
        split_trigger_priority(source.priority.clone().or_else(|| trigger.priority.clone()));
    let mut kind_specific = trigger.kind_specific.clone();
    kind_specific.extend(source.kind_specific.clone());
    let mut secrets = trigger.secrets.clone();
    secrets.extend(source.secrets.clone());
    let source_label = source
        .id
        .clone()
        .unwrap_or_else(|| format!("source-{}", source_index + 1));
    ResolvedTriggerConfig {
        id: format!("{}.{}", trigger.id, source_label),
        kind: source.kind,
        provider: source.provider.clone(),
        autonomy_tier: trigger.autonomy_tier,
        match_: source.match_.clone().unwrap_or_default(),
        when: trigger.when.clone(),
        when_budget: trigger.when_budget.clone(),
        handler: trigger.handler.clone(),
        dedupe_key: source
            .dedupe_key
            .clone()
            .or_else(|| trigger.dedupe_key.clone()),
        retry: source
            .retry
            .clone()
            .unwrap_or_else(|| trigger.retry.clone()),
        dispatch_priority,
        budget: source
            .budget
            .clone()
            .unwrap_or_else(|| trigger.budget.clone()),
        concurrency: source
            .concurrency
            .clone()
            .or_else(|| trigger.concurrency.clone()),
        throttle: source.throttle.clone().or_else(|| trigger.throttle.clone()),
        rate_limit: source
            .rate_limit
            .clone()
            .or_else(|| trigger.rate_limit.clone()),
        debounce: source.debounce.clone().or_else(|| trigger.debounce.clone()),
        singleton: source
            .singleton
            .clone()
            .or_else(|| trigger.singleton.clone()),
        batch: source.batch.clone().or_else(|| trigger.batch.clone()),
        window: source.window.clone().or_else(|| trigger.window.clone()),
        priority_flow,
        secrets,
        filter: source.filter.clone().or_else(|| trigger.filter.clone()),
        kind_specific,
        manifest_dir: manifest_dir.to_path_buf(),
        manifest_path: manifest_path.to_path_buf(),
        package_name,
        exports,
        table_index,
        shape_error: source
            .match_
            .is_none()
            .then(|| format!("trigger source '{source_label}' missing match")),
    }
}

pub(crate) fn resolved_provider_connectors_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedProviderConnectorConfig> {
    manifest
        .providers
        .iter()
        .map(|provider| {
            let connector = match (
                provider.connector.harn.as_deref(),
                provider.connector.rust.as_deref(),
            ) {
                (Some(module), None) => ResolvedProviderConnectorKind::Harn {
                    module: module.to_string(),
                },
                (None, Some("builtin")) | (None, None) => {
                    ResolvedProviderConnectorKind::RustBuiltin
                }
                (None, Some(other)) => ResolvedProviderConnectorKind::Invalid(format!(
                    "provider '{}' uses unsupported connector.rust value '{other}'",
                    provider.id.as_str()
                )),
                (Some(_), Some(_)) => ResolvedProviderConnectorKind::Invalid(format!(
                    "provider '{}' cannot set both connector.harn and connector.rust",
                    provider.id.as_str()
                )),
            };
            ResolvedProviderConnectorConfig {
                id: provider.id.clone(),
                manifest_dir: manifest_dir.to_path_buf(),
                connector,
                oauth: provider.oauth.clone(),
            }
        })
        .collect()
}

pub(crate) fn split_trigger_priority(
    priority: Option<TriggerPriorityField>,
) -> (TriggerDispatchPriority, Option<TriggerPriorityManifestSpec>) {
    match priority {
        Some(TriggerPriorityField::Dispatch(priority)) => (priority, None),
        Some(TriggerPriorityField::Flow(spec)) => (TriggerDispatchPriority::Normal, Some(spec)),
        None => (TriggerDispatchPriority::Normal, None),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TriggerFunctionSignature {
    pub(crate) params: Vec<Option<harn_parser::TypeExpr>>,
    pub(crate) return_type: Option<harn_parser::TypeExpr>,
}

pub(crate) fn manifest_trigger_location(trigger: &ResolvedTriggerConfig) -> String {
    format!(
        "{} [[triggers]] table #{} (id = {})",
        trigger.manifest_path.display(),
        trigger.table_index + 1,
        trigger.id
    )
}

pub(crate) fn trigger_error(trigger: &ResolvedTriggerConfig, message: impl Into<String>) -> String {
    format!("{}: {}", manifest_trigger_location(trigger), message.into())
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(crate) fn parse_local_trigger_ref(
    raw: &str,
    field_name: &str,
    trigger: &ResolvedTriggerConfig,
) -> Result<TriggerFunctionRef, String> {
    if raw.trim().is_empty() {
        return Err(trigger_error(
            trigger,
            format!("{field_name} cannot be empty"),
        ));
    }
    if raw.contains("://") {
        return Err(trigger_error(
            trigger,
            format!("{field_name} must reference a local function, not a URI"),
        ));
    }
    if let Some((module_name, function_name)) = raw.rsplit_once("::") {
        if module_name.trim().is_empty() || function_name.trim().is_empty() {
            return Err(trigger_error(
                trigger,
                format!("{field_name} must use <module>::<function> when module-qualified"),
            ));
        }
        if !valid_identifier(function_name) {
            return Err(trigger_error(
                trigger,
                format!("{field_name} function name '{function_name}' is not a valid identifier"),
            ));
        }
        return Ok(TriggerFunctionRef {
            raw: raw.to_string(),
            module_name: Some(module_name.to_string()),
            function_name: function_name.to_string(),
        });
    }
    if !valid_identifier(raw) {
        return Err(trigger_error(
            trigger,
            format!("{field_name} '{raw}' is not a valid bare function identifier"),
        ));
    }
    Ok(TriggerFunctionRef {
        raw: raw.to_string(),
        module_name: None,
        function_name: raw.to_string(),
    })
}

pub(crate) fn parse_trigger_handler_uri(
    trigger: &ResolvedTriggerConfig,
) -> Result<TriggerHandlerUri, String> {
    let raw = trigger.handler.trim();
    if let Some(target) = raw.strip_prefix("a2a://") {
        if target.is_empty() {
            return Err(trigger_error(
                trigger,
                "handler a2a:// target cannot be empty",
            ));
        }
        let allow_cleartext = extract_kind_field(trigger, "allow_cleartext")
            .map(parse_trigger_allow_cleartext)
            .transpose()?
            .unwrap_or(false);
        return Ok(TriggerHandlerUri::A2a {
            target: target.to_string(),
            allow_cleartext,
        });
    }
    if let Some(queue) = raw.strip_prefix("worker://") {
        if queue.is_empty() {
            return Err(trigger_error(
                trigger,
                "handler worker:// queue cannot be empty",
            ));
        }
        return Ok(TriggerHandlerUri::Worker {
            queue: queue.to_string(),
        });
    }
    if raw.contains("://") {
        return Err(trigger_error(
            trigger,
            format!("handler URI scheme in '{raw}' is not implemented"),
        ));
    }
    Ok(TriggerHandlerUri::Local(parse_local_trigger_ref(
        raw, "handler", trigger,
    )?))
}

pub(crate) fn parse_secret_id(raw: &str) -> Option<harn_vm::secrets::SecretId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, version_text)) => {
            let version = version_text.parse::<u64>().ok()?;
            (base, harn_vm::secrets::SecretVersion::Exact(version))
        }
        None => (trimmed, harn_vm::secrets::SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(harn_vm::secrets::SecretId::new(namespace, name).with_version(version))
}

pub(crate) fn extract_kind_field<'a>(
    trigger: &'a ResolvedTriggerConfig,
    field: &str,
) -> Option<&'a toml::Value> {
    trigger.kind_specific.get(field)
}

pub(crate) fn looks_like_utc_offset_timezone(raw: &str) -> bool {
    let value = raw.trim();
    if let Some(rest) = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("utc"))
        .or_else(|| value.strip_prefix("GMT"))
        .or_else(|| value.strip_prefix("gmt"))
    {
        return rest.starts_with('+') || rest.starts_with('-');
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 3 || !matches!(chars[0], '+' | '-') {
        return false;
    }
    chars[1..]
        .iter()
        .all(|ch| ch.is_ascii_digit() || *ch == ':')
}

pub(crate) fn parse_jmespath_expression(
    trigger: &ResolvedTriggerConfig,
    field_name: &str,
    expr: &str,
) -> Result<(), String> {
    jmespath::compile(expr).map(|_| ()).map_err(|error| {
        trigger_error(
            trigger,
            format!("{field_name} '{expr}' is invalid: {error}"),
        )
    })
}

pub(crate) fn parse_duration_millis(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let (value, unit) = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| (&trimmed[..index], &trimmed[index..]))
        .unwrap_or((trimmed, "ms"));
    let amount = value
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{raw}'"))?;
    let multiplier = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => {
            return Err(format!(
                "invalid duration unit in '{raw}'; expected ms, s, m, or h"
            ))
        }
    };
    Ok(amount.saturating_mul(multiplier))
}

pub(crate) fn validate_static_trigger_config(
    trigger: &ResolvedTriggerConfig,
) -> Result<(), String> {
    if let Some(message) = &trigger.shape_error {
        return Err(trigger_error(trigger, message));
    }
    if trigger.id.trim().is_empty() {
        return Err(trigger_error(trigger, "id cannot be empty"));
    }
    let Some(provider_metadata) = harn_vm::provider_metadata(trigger.provider.as_str()) else {
        return Err(trigger_error(
            trigger,
            format!("provider '{}' is not registered", trigger.provider.as_str()),
        ));
    };
    let kind_name = trigger_kind_label(trigger.kind);
    if !provider_metadata.supports_kind(kind_name) {
        return Err(trigger_error(
            trigger,
            format!(
                "provider '{}' does not support trigger kind '{}'",
                trigger.provider.as_str(),
                kind_name
            ),
        ));
    }
    for secret_name in provider_metadata.required_secret_names() {
        if !trigger.secrets.contains_key(secret_name) {
            return Err(trigger_error(
                trigger,
                format!(
                    "provider '{}' requires secret '{}'",
                    trigger.provider.as_str(),
                    secret_name
                ),
            ));
        }
    }
    if let Some(dedupe_key) = &trigger.dedupe_key {
        parse_jmespath_expression(trigger, "dedupe_key", dedupe_key)?;
    }
    if let Some(filter) = &trigger.filter {
        parse_jmespath_expression(trigger, "filter", filter)?;
    }
    if let Some(value) = extract_kind_field(trigger, "allow_cleartext") {
        let _ = parse_trigger_allow_cleartext(value)?;
        if !trigger.handler.trim().starts_with("a2a://") {
            return Err(trigger_error(
                trigger,
                "`allow_cleartext` is only valid for `a2a://...` handlers",
            ));
        }
    }
    if trigger.when_budget.is_some() && trigger.when.is_none() {
        return Err(trigger_error(
            trigger,
            "when_budget requires a when predicate",
        ));
    }
    if let Some(daily_cost_usd) = trigger.budget.daily_cost_usd {
        if daily_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.daily_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if let Some(hourly_cost_usd) = trigger.budget.hourly_cost_usd {
        if hourly_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.hourly_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if trigger.budget.max_autonomous_decisions_per_hour == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_autonomous_decisions_per_hour must be greater than or equal to 1",
        ));
    }
    if trigger.budget.max_autonomous_decisions_per_day == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_autonomous_decisions_per_day must be greater than or equal to 1",
        ));
    }
    if let Some(max_cost_usd) = trigger.budget.max_cost_usd {
        if max_cost_usd.is_sign_negative() {
            return Err(trigger_error(
                trigger,
                "budget.max_cost_usd must be greater than or equal to 0",
            ));
        }
    }
    if trigger.budget.max_tokens == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_tokens must be greater than or equal to 1",
        ));
    }
    if trigger.budget.max_concurrent == Some(0) {
        return Err(trigger_error(
            trigger,
            "budget.max_concurrent must be greater than or equal to 1",
        ));
    }
    if let Some(when_budget) = trigger.when_budget.as_ref() {
        if when_budget
            .max_cost_usd
            .is_some_and(|value| value.is_sign_negative())
        {
            return Err(trigger_error(
                trigger,
                "when_budget.max_cost_usd must be greater than or equal to 0",
            ));
        }
        if when_budget.tokens_max == Some(0) {
            return Err(trigger_error(
                trigger,
                "when_budget.tokens_max must be greater than or equal to 1",
            ));
        }
        if let Some(timeout) = when_budget.timeout.as_deref() {
            parse_duration_millis(timeout)
                .map_err(|error| trigger_error(trigger, format!("when_budget.timeout {error}")))?;
        }
    }
    if trigger.retry.max > TRIGGER_RETRY_MAX_LIMIT {
        return Err(trigger_error(
            trigger,
            format!("retry.max must be less than or equal to {TRIGGER_RETRY_MAX_LIMIT}"),
        ));
    }
    if trigger.retry.retention_days == 0 {
        return Err(trigger_error(
            trigger,
            "retry.retention_days must be greater than or equal to 1",
        ));
    }
    if let Some(spec) = &trigger.concurrency {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "concurrency.max must be greater than or equal to 1",
            ));
        }
    }
    if let Some(spec) = &trigger.throttle {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "throttle.max must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("throttle.period {error}")))?;
    }
    if let Some(spec) = &trigger.rate_limit {
        if spec.max == 0 {
            return Err(trigger_error(
                trigger,
                "rate_limit.max must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("rate_limit.period {error}")))?;
    }
    if let Some(spec) = &trigger.debounce {
        harn_vm::parse_flow_control_duration(&spec.period)
            .map_err(|error| trigger_error(trigger, format!("debounce.period {error}")))?;
    }
    if let Some(spec) = &trigger.batch {
        if spec.size == 0 {
            return Err(trigger_error(
                trigger,
                "batch.size must be greater than or equal to 1",
            ));
        }
        harn_vm::parse_flow_control_duration(&spec.timeout)
            .map_err(|error| trigger_error(trigger, format!("batch.timeout {error}")))?;
    }
    if let Some(spec) = &trigger.priority_flow {
        if spec.order.is_empty() {
            return Err(trigger_error(
                trigger,
                "priority.order must contain at least one value",
            ));
        }
    }
    if trigger.priority_flow.is_some()
        && trigger.concurrency.is_none()
        && trigger.budget.max_concurrent.is_none()
    {
        return Err(trigger_error(
            trigger,
            "priority requires concurrency.max so queued dispatches have a slot to compete for",
        ));
    }
    if trigger.batch.is_some()
        && (trigger.debounce.is_some()
            || trigger.singleton.is_some()
            || trigger.concurrency.is_some()
            || trigger.priority_flow.is_some()
            || trigger.throttle.is_some()
            || trigger.rate_limit.is_some()
            || trigger.budget.max_concurrent.is_some())
    {
        return Err(trigger_error(
            trigger,
            "batch cannot currently be combined with debounce, singleton, concurrency, priority, throttle, or rate_limit",
        ));
    }
    for (name, secret_ref) in &trigger.secrets {
        let Some(secret_id) = parse_secret_id(secret_ref) else {
            return Err(trigger_error(
                trigger,
                format!("secret '{name}' must use <namespace>/<name> syntax"),
            ));
        };
        if secret_id.namespace != trigger.provider.as_str() {
            return Err(trigger_error(
                trigger,
                format!(
                    "secret '{name}' uses namespace '{}' but provider is '{}'",
                    secret_id.namespace,
                    trigger.provider.as_str()
                ),
            ));
        }
    }
    if matches!(trigger.kind, TriggerKind::Cron) {
        let Some(schedule) = extract_kind_field(trigger, "schedule").and_then(toml::Value::as_str)
        else {
            return Err(trigger_error(
                trigger,
                "cron triggers require a string schedule field",
            ));
        };
        croner::Cron::from_str(schedule).map_err(|error| {
            trigger_error(
                trigger,
                format!("invalid cron schedule '{schedule}': {error}"),
            )
        })?;
        if let Some(timezone) =
            extract_kind_field(trigger, "timezone").and_then(toml::Value::as_str)
        {
            if looks_like_utc_offset_timezone(timezone) {
                return Err(trigger_error(
                    trigger,
                    format!(
                        "invalid cron timezone '{timezone}': use an IANA timezone name like 'America/New_York', not a UTC offset"
                    ),
                ));
            }
            timezone.parse::<Tz>().map_err(|error| {
                trigger_error(
                    trigger,
                    format!("invalid cron timezone '{timezone}': {error}"),
                )
            })?;
        }
    }
    if matches!(trigger.kind, TriggerKind::Stream) {
        validate_stream_trigger_config(trigger)?;
    } else if trigger.window.is_some() {
        return Err(trigger_error(
            trigger,
            "window is only valid for stream triggers",
        ));
    }
    Ok(())
}

pub(crate) fn validate_orchestrator_budget(manifest: Option<&Manifest>) -> Result<(), String> {
    let Some(manifest) = manifest else {
        return Ok(());
    };
    if manifest
        .orchestrator
        .budget
        .daily_cost_usd
        .is_some_and(|value| value.is_sign_negative())
    {
        return Err(
            "orchestrator.budget.daily_cost_usd must be greater than or equal to 0".to_string(),
        );
    }
    if manifest
        .orchestrator
        .budget
        .hourly_cost_usd
        .is_some_and(|value| value.is_sign_negative())
    {
        return Err(
            "orchestrator.budget.hourly_cost_usd must be greater than or equal to 0".to_string(),
        );
    }
    Ok(())
}

pub(crate) fn validate_stream_trigger_config(
    trigger: &ResolvedTriggerConfig,
) -> Result<(), String> {
    if let Some(window) = &trigger.window {
        validate_stream_window(trigger, window)?;
    }
    let provider = trigger.provider.as_str();
    let has_any = |fields: &[&str]| {
        fields.iter().any(|field| {
            extract_kind_field(trigger, field).is_some_and(|value| {
                value.as_str().is_some_and(|text| !text.trim().is_empty())
                    || value.as_array().is_some_and(|items| !items.is_empty())
                    || value.as_table().is_some_and(|table| !table.is_empty())
            })
        })
    };
    let required = match provider {
        "kafka" => (!has_any(&["topic", "topics"])).then_some("topic or topics"),
        "nats" => (!has_any(&["subject", "subjects"])).then_some("subject or subjects"),
        "pulsar" => (!has_any(&["topic", "topics"])).then_some("topic or topics"),
        "postgres-cdc" => (!has_any(&["slot"])).then_some("slot"),
        "email" => {
            (!has_any(&["address", "domain", "routing"])).then_some("address, domain, or routing")
        }
        "websocket" => (!has_any(&["url", "path"])).then_some("url or path"),
        _ => None,
    };
    if let Some(required) = required {
        return Err(trigger_error(
            trigger,
            format!("stream provider '{provider}' requires {required}"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_stream_window(
    trigger: &ResolvedTriggerConfig,
    window: &TriggerStreamWindowManifestSpec,
) -> Result<(), String> {
    if window.max_items == Some(0) {
        return Err(trigger_error(
            trigger,
            "window.max_items must be greater than or equal to 1",
        ));
    }
    if let Some(size) = window.size.as_deref() {
        harn_vm::parse_flow_control_duration(size)
            .map_err(|error| trigger_error(trigger, format!("window.size {error}")))?;
    }
    if let Some(every) = window.every.as_deref() {
        harn_vm::parse_flow_control_duration(every)
            .map_err(|error| trigger_error(trigger, format!("window.every {error}")))?;
    }
    if let Some(gap) = window.gap.as_deref() {
        harn_vm::parse_flow_control_duration(gap)
            .map_err(|error| trigger_error(trigger, format!("window.gap {error}")))?;
    }
    match window.mode {
        TriggerStreamWindowMode::Tumbling => {
            if window.size.is_none() {
                return Err(trigger_error(
                    trigger,
                    "tumbling stream windows require window.size",
                ));
            }
            if window.every.is_some() || window.gap.is_some() {
                return Err(trigger_error(
                    trigger,
                    "tumbling stream windows cannot set window.every or window.gap",
                ));
            }
        }
        TriggerStreamWindowMode::Sliding => {
            if window.size.is_none() || window.every.is_none() {
                return Err(trigger_error(
                    trigger,
                    "sliding stream windows require window.size and window.every",
                ));
            }
            if window.gap.is_some() {
                return Err(trigger_error(
                    trigger,
                    "sliding stream windows cannot set window.gap",
                ));
            }
        }
        TriggerStreamWindowMode::Session => {
            if window.gap.is_none() {
                return Err(trigger_error(
                    trigger,
                    "session stream windows require window.gap",
                ));
            }
            if window.every.is_some() {
                return Err(trigger_error(
                    trigger,
                    "session stream windows cannot set window.every",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_static_trigger_configs(
    triggers: &[ResolvedTriggerConfig],
) -> Result<(), String> {
    let mut seen_ids = HashSet::new();
    for trigger in triggers {
        validate_static_trigger_config(trigger)?;
        if !seen_ids.insert(trigger.id.clone()) {
            return Err(trigger_error(
                trigger,
                format!(
                    "duplicate trigger id '{}' across loaded manifests",
                    trigger.id
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn parse_trigger_allow_cleartext(value: &toml::Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| "`allow_cleartext` must be a boolean".to_string())
}

pub(crate) fn manifest_module_source_path(
    manifest_dir: &Path,
    package_name: Option<&str>,
    exports: &HashMap<String, String>,
    module_name: Option<&str>,
) -> Result<PathBuf, String> {
    match module_name {
        None => {
            let path = manifest_dir.join("lib.harn");
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "no lib.harn found next to manifest in {}",
                    manifest_dir.display()
                ))
            }
        }
        Some(module_name) if package_name.is_some_and(|pkg| pkg == module_name) => {
            let path = manifest_dir.join("lib.harn");
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "module '{}' resolves to local lib.harn, but {} is missing",
                    module_name,
                    path.display()
                ))
            }
        }
        Some(module_name) if exports.contains_key(module_name) => {
            let rel_path = exports.get(module_name).expect("checked export key exists");
            let path = manifest_dir.join(rel_path);
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "export '{}' resolves to {}, but that path does not exist",
                    module_name,
                    path.display()
                ))
            }
        }
        Some(module_name) => {
            let path = harn_vm::resolve_module_import_path(manifest_dir, module_name);
            if path.exists() {
                Ok(path)
            } else {
                Err(format!(
                    "module '{}' could not be resolved from {}",
                    module_name,
                    manifest_dir.display()
                ))
            }
        }
    }
}

pub(crate) fn load_trigger_function_signatures(
    path: &Path,
) -> Result<BTreeMap<String, TriggerFunctionSignature>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let program = harn_parser::parse_source(&source)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let mut signatures = BTreeMap::new();
    for node in &program {
        let (_, inner) = harn_parser::peel_attributes(node);
        if let harn_parser::Node::FnDecl {
            name,
            params,
            return_type,
            ..
        } = &inner.node
        {
            signatures.insert(
                name.clone(),
                TriggerFunctionSignature {
                    params: params.iter().map(|param| param.type_expr.clone()).collect(),
                    return_type: return_type.clone(),
                },
            );
        }
    }
    Ok(signatures)
}

pub(crate) async fn resolve_manifest_exports(
    vm: &mut harn_vm::Vm,
    manifest_dir: &Path,
    package_name: Option<&str>,
    exports: &HashMap<String, String>,
    module_name: Option<&str>,
) -> Result<ManifestModuleExports, String> {
    match module_name {
        None => {
            let lib_path = manifest_module_source_path(manifest_dir, package_name, exports, None)?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) if package_name.is_some_and(|name| name == module_name) => {
            let lib_path = manifest_module_source_path(
                manifest_dir,
                package_name,
                exports,
                Some(module_name),
            )?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) if exports.contains_key(module_name) => {
            let lib_path = manifest_module_source_path(
                manifest_dir,
                package_name,
                exports,
                Some(module_name),
            )?;
            vm.load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string())
        }
        Some(module_name) => vm
            .load_module_exports_from_import(module_name)
            .await
            .map_err(|error| error.to_string()),
    }
}

pub(crate) struct ManifestExtensionProviderSchema {
    provider_id: &'static str,
    schema_name: &'static str,
    metadata: harn_vm::ProviderMetadata,
}

impl harn_vm::ProviderSchema for ManifestExtensionProviderSchema {
    fn provider_id(&self) -> &'static str {
        self.provider_id
    }

    fn harn_schema_name(&self) -> &'static str {
        self.schema_name
    }

    fn metadata(&self) -> harn_vm::ProviderMetadata {
        self.metadata.clone()
    }

    fn normalize(
        &self,
        _kind: &str,
        _headers: &BTreeMap<String, String>,
        raw: serde_json::Value,
    ) -> Result<harn_vm::ProviderPayload, harn_vm::ProviderCatalogError> {
        Ok(harn_vm::ProviderPayload::Extension(
            harn_vm::triggers::ExtensionProviderPayload {
                provider: self.metadata.provider.clone(),
                schema_name: self.metadata.schema_name.clone(),
                raw,
            },
        ))
    }
}

pub(crate) fn leak_static_string(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

pub(crate) async fn install_manifest_provider_schemas(
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    let mut schemas: Vec<Arc<dyn harn_vm::ProviderSchema>> = Vec::new();
    for provider in &extensions.provider_connectors {
        match &provider.connector {
            ResolvedProviderConnectorKind::RustBuiltin => continue,
            ResolvedProviderConnectorKind::Invalid(message) => {
                return Err(message.clone());
            }
            ResolvedProviderConnectorKind::Harn { module } => {
                let module_path =
                    harn_vm::resolve_module_import_path(&provider.manifest_dir, module);
                let contract = harn_vm::connectors::harn_module::load_contract(&module_path)
                    .await
                    .map_err(|error| {
                        format!(
                            "failed to load connector module '{}' for provider '{}': {error}",
                            module_path.display(),
                            provider.id.as_str()
                        )
                    })?;
                if contract.provider_id != provider.id {
                    return Err(format!(
                        "provider '{}' resolves to connector module '{}' which declares provider_id '{}'",
                        provider.id.as_str(),
                        module_path.display(),
                        contract.provider_id.as_str()
                    ));
                }
                let metadata = harn_vm::ProviderMetadata {
                    provider: contract.provider_id.as_str().to_string(),
                    kinds: contract
                        .kinds
                        .iter()
                        .map(|kind| kind.as_str().to_string())
                        .collect(),
                    schema_name: contract.payload_schema.harn_schema_name.clone(),
                    runtime: harn_vm::ProviderRuntimeMetadata::Placeholder,
                    ..harn_vm::ProviderMetadata::default()
                };
                let schema = ManifestExtensionProviderSchema {
                    provider_id: leak_static_string(metadata.provider.clone()),
                    schema_name: leak_static_string(metadata.schema_name.clone()),
                    metadata,
                };
                schemas.push(Arc::new(schema));
            }
        }
    }
    harn_vm::reset_provider_catalog_with(schemas).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn is_trigger_event_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "TriggerEvent")
}

pub(crate) fn is_bool_type(ty: &harn_parser::TypeExpr) -> bool {
    matches!(ty, harn_parser::TypeExpr::Named(name) if name == "bool")
}

pub(crate) fn is_predicate_return_type(ty: &harn_parser::TypeExpr) -> bool {
    if is_bool_type(ty) {
        return true;
    }
    matches!(
        ty,
        harn_parser::TypeExpr::Applied { name, args }
            if name == "Result"
                && args.len() == 2
                && args.first().is_some_and(is_bool_type)
    )
}
