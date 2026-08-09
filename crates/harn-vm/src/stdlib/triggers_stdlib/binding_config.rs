//! Parsing the `trigger_register` configuration dict into a [`TriggerBindingSpec`].
//!
//! This is the trigger DSL's front door: match rules, autonomy tier, budgets and
//! their exhaustion strategy, retry policy, batching, and the `handler:` field —
//! which is uniformly a closure, a `a2a://`/`worker://` URI, or a handler-variant
//! dict (`spawn_to_pool`, `reminder_inject`, `interrupt_and_suspend`). It also
//! computes the definition fingerprint the registry versions bindings by.

use crate::duration_parse::DurationParseError;
use crate::triggers::dispatcher::DEFAULT_MAX_ATTEMPTS;
use crate::triggers::registry::{AgentScope, TargetExpr};
use crate::triggers::{
    RetryPolicy, TriggerBindingSource, TriggerBindingSpec, TriggerHandlerSpec,
    TriggerPredicateSpec, TriggerRetryConfig,
};
use crate::trust_graph::AutonomyTier;
use crate::value::{VmError, VmValue};
use crate::TriggerPredicateBudget;

use super::args::{
    number_value, optional_bool, optional_string, parse_string_list, required_string,
};
use super::trust::parse_autonomy_tier;

pub(super) fn parse_trigger_config(
    config: &crate::value::DictMap,
) -> Result<TriggerBindingSpec, VmError> {
    let id = optional_string(config, "id").unwrap_or_default();
    let kind = required_string(config, "kind", "trigger_register")?;
    let provider =
        crate::ProviderId::from(required_string(config, "provider", "trigger_register")?);
    let allow_cleartext =
        optional_bool(config, "allow_cleartext", "trigger_register")?.unwrap_or(false);
    let (handler, handler_descriptor) = parse_handler_value(
        config
            .get("handler")
            .ok_or_else(|| VmError::Runtime("trigger_register: missing `handler`".to_string()))?,
        "trigger_register",
        "handler",
        allow_cleartext,
    )?;
    let when = match config.get("when") {
        Some(VmValue::Nil) | None => None,
        Some(value) => {
            let (handler, _) = parse_handler_value(value, "trigger_register", "when", false)?;
            match handler {
                TriggerHandlerSpec::Local { raw, callable } => {
                    Some(TriggerPredicateSpec { raw, callable })
                }
                _ => {
                    return Err(VmError::Runtime(
                        "trigger_register: `when` must be a closure".to_string(),
                    ))
                }
            }
        }
    };
    let match_events = config
        .get("match")
        .and_then(|value| match value {
            VmValue::Dict(map) => map.get("events"),
            _ => None,
        })
        .or_else(|| config.get("events"))
        .map(parse_string_list)
        .transpose()?
        .unwrap_or_default();
    let autonomy_tier = match config.get("autonomy_tier").or_else(|| config.get("tier")) {
        Some(VmValue::Nil) | None => AutonomyTier::default(),
        Some(value) => parse_autonomy_tier(value)?,
    };
    let budget = config.get("budget").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        _ => None,
    });
    let when_budget = config.get("when_budget").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        VmValue::Nil => None,
        _ => None,
    });
    let retry = config.get("retry").and_then(|value| match value {
        VmValue::Dict(map) => Some(map),
        VmValue::Nil => None,
        _ => None,
    });
    let dedupe_key = optional_string(config, "dedupe_key");
    let filter = optional_string(config, "filter");
    let daily_cost_usd = budget
        .and_then(|map| map.get("daily_cost_usd"))
        .and_then(number_value);
    let hourly_cost_usd = budget
        .and_then(|map| map.get("hourly_cost_usd"))
        .and_then(number_value);
    let max_autonomous_decisions_per_hour = budget
        .and_then(|map| map.get("max_autonomous_decisions_per_hour"))
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64);
    let max_autonomous_decisions_per_day = budget
        .and_then(|map| map.get("max_autonomous_decisions_per_day"))
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64);
    let on_budget_exhausted = match budget.and_then(|map| map.get("on_budget_exhausted")) {
        Some(VmValue::String(text)) => match text.as_str() {
            "false" => crate::TriggerBudgetExhaustionStrategy::False,
            "retry_later" => crate::TriggerBudgetExhaustionStrategy::RetryLater,
            "fail" => crate::TriggerBudgetExhaustionStrategy::Fail,
            "warn" => crate::TriggerBudgetExhaustionStrategy::Warn,
            raw => {
                return Err(VmError::Runtime(format!(
                    "trigger_register: unsupported budget.on_budget_exhausted '{raw}'"
                )))
            }
        },
        Some(_) => {
            return Err(VmError::Runtime(
                "trigger_register: budget.on_budget_exhausted must be a string".to_string(),
            ))
        }
        None => crate::TriggerBudgetExhaustionStrategy::False,
    };
    let max_concurrent = budget
        .and_then(|map| map.get("max_concurrent"))
        .and_then(VmValue::as_int)
        .map(|value| value as u32);
    if max_autonomous_decisions_per_hour == Some(0) {
        return Err(VmError::Runtime(
            "trigger_register: budget.max_autonomous_decisions_per_hour must be greater than or equal to 1"
                .to_string(),
        ));
    }
    if max_autonomous_decisions_per_day == Some(0) {
        return Err(VmError::Runtime(
            "trigger_register: budget.max_autonomous_decisions_per_day must be greater than or equal to 1"
                .to_string(),
        ));
    }
    let when_budget = when_budget
        .map(|map| {
            Ok::<TriggerPredicateBudget, VmError>(TriggerPredicateBudget {
                max_cost_usd: map.get("max_cost_usd").and_then(number_value),
                tokens_max: map
                    .get("tokens_max")
                    .and_then(VmValue::as_int)
                    .map(|value| value.max(0) as u64),
                timeout_ms: map
                    .get("timeout")
                    .and_then(|value| match value {
                        VmValue::String(text) => Some(text.to_string()),
                        _ => None,
                    })
                    .map(|text| parse_duration_millis(&text))
                    .transpose()?,
            })
        })
        .transpose()?;
    let when_budget = {
        let mut merged = when_budget;
        if let Some(map) = budget {
            let max_cost_usd = map.get("max_cost_usd").and_then(number_value);
            let max_tokens = map
                .get("max_tokens")
                .or_else(|| map.get("tokens_max"))
                .and_then(VmValue::as_int)
                .map(|value| value.max(0) as u64);
            if max_cost_usd.is_some() || max_tokens.is_some() {
                let budget = merged.get_or_insert_with(TriggerPredicateBudget::default);
                if budget.max_cost_usd.is_none() {
                    budget.max_cost_usd = max_cost_usd;
                }
                if budget.tokens_max.is_none() {
                    budget.tokens_max = max_tokens;
                }
            }
        }
        merged
    };
    if when_budget.is_some() && when.is_none() {
        return Err(VmError::Runtime(
            "trigger_register: when_budget requires a when predicate".to_string(),
        ));
    }
    let manifest_path = optional_string(config, "manifest_path").map(std::path::PathBuf::from);
    let package_name = optional_string(config, "package_name");
    let retry = parse_retry_config(retry.map(|value| &**value), "trigger_register")?;

    // CH-04 (#1875): parse the optional `batch` aggregation field.
    let aggregation = match config.get("batch") {
        Some(value) => crate::triggers::aggregation::parse_aggregation_config(value)?,
        None => None,
    };
    let aggregation_fingerprint = aggregation.as_ref().map(|cfg| {
        serde_json::json!({
            "count": cfg.count,
            "window_ms": cfg.window.as_millis() as u64,
            "key": cfg.key_path,
            "expire_action": cfg.expire_action.as_str(),
        })
    });

    let fingerprint = serde_json::to_string(&serde_json::json!({
        "id": id,
        "kind": kind,
        "provider": provider.as_str(),
        "autonomy_tier": autonomy_tier,
        "handler": handler_descriptor,
        "when": when.as_ref().map(|predicate| predicate.raw.clone()),
        "when_budget": when_budget,
        "retry": {
            "max": retry.max_attempts(),
            "policy": format!("{:?}", retry.policy),
        },
        "match_events": match_events,
        "dedupe_key": dedupe_key,
        "filter": filter,
        "allow_cleartext": allow_cleartext,
        "daily_cost_usd": daily_cost_usd,
        "hourly_cost_usd": hourly_cost_usd,
        "max_autonomous_decisions_per_hour": max_autonomous_decisions_per_hour,
        "max_autonomous_decisions_per_day": max_autonomous_decisions_per_day,
        "on_budget_exhausted": on_budget_exhausted.as_str(),
        "max_concurrent": max_concurrent,
        "aggregation": aggregation_fingerprint,
        "manifest_path": manifest_path.as_ref().map(|path| path.display().to_string()),
        "package_name": package_name,
    }))
    .unwrap_or_else(|_| format!("{}:{}:{}", id, kind, provider.as_str()));

    // CH-02 (#1872): channel-source triggers parse their selector strings
    // at registration so malformed selectors fail loudly before any emit.
    if provider.as_str() == "channel" {
        if match_events.is_empty() {
            return Err(VmError::Runtime(
                "trigger_register: provider=\"channel\" requires `match.events: [\"channel:<scope>:<name>\"]`"
                    .to_string(),
            ));
        }
        for selector in &match_events {
            crate::channels::ChannelSelector::parse(selector).map_err(|error| {
                VmError::Runtime(format!(
                    "trigger_register: invalid channel selector: {error}"
                ))
            })?;
        }
    }

    Ok(TriggerBindingSpec {
        id,
        source: TriggerBindingSource::Dynamic,
        kind,
        provider,
        autonomy_tier,
        handler,
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when,
        when_budget,
        retry,
        match_events,
        dedupe_key,
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter,
        daily_cost_usd,
        hourly_cost_usd,
        max_autonomous_decisions_per_hour,
        max_autonomous_decisions_per_day,
        on_budget_exhausted,
        max_concurrent,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation,
        manifest_path,
        package_name,
        definition_fingerprint: fingerprint,
    })
}

pub(crate) fn validate_resume_trigger_spec(config: &crate::value::DictMap) -> Result<(), VmError> {
    let mut normalized = config.clone();
    normalized
        .entry(crate::value::intern_key("handler"))
        .or_insert_with(|| {
            VmValue::String(arcstr::ArcStr::from("worker://__resume_auto_resume__"))
        });
    parse_trigger_config(&normalized).map(|_| ())
}

fn parse_duration_millis(raw: &str) -> Result<u64, VmError> {
    // The CLI manifest validator parses this same field via
    // `package::validation::parse_duration_millis`; both now share one grammar,
    // so a `when_budget.timeout` cannot be accepted here and rejected there.
    crate::duration_parse::parse_millis(raw).map_err(|error| {
        VmError::Runtime(match error {
            DurationParseError::Empty => {
                "trigger_register: when_budget.timeout cannot be empty".to_string()
            }
            DurationParseError::MissingUnit => format!(
                "trigger_register: when_budget.timeout '{raw}' must include a unit suffix; expected ms, s, m, h, d, or w"
            ),
            DurationParseError::UnknownUnit(_) => {
                format!("trigger_register: unsupported when_budget.timeout unit in '{raw}'")
            }
            DurationParseError::TooLarge => {
                format!("trigger_register: when_budget.timeout '{raw}' is too large")
            }
            DurationParseError::NoDigits | DurationParseError::AmountOverflow => {
                format!("trigger_register: invalid when_budget.timeout '{raw}'")
            }
        })
    })
}

fn parse_retry_config(
    retry: Option<&crate::value::DictMap>,
    builtin: &str,
) -> Result<TriggerRetryConfig, VmError> {
    let Some(retry) = retry else {
        return Ok(TriggerRetryConfig::default());
    };
    let max = retry
        .get("max")
        .and_then(VmValue::as_int)
        .unwrap_or(DEFAULT_MAX_ATTEMPTS as i64)
        .max(1) as u32;
    let policy = match optional_string(retry, "backoff").as_deref() {
        None | Some("svix") => RetryPolicy::Svix,
        Some("immediate") => RetryPolicy::Linear { delay_ms: 0 },
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: unsupported retry.backoff '{other}', expected 'svix' or 'immediate'"
            )))
        }
    };
    Ok(TriggerRetryConfig::new(max, policy))
}

fn parse_handler_value(
    value: &VmValue,
    builtin: &str,
    field_name: &str,
    allow_cleartext: bool,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    match value {
        VmValue::Closure(closure) => {
            let raw = closure.func.name.clone();
            Ok((
                TriggerHandlerSpec::Local {
                    raw: raw.clone(),
                    callable: crate::value::VmCallable::Eager(closure.clone()),
                },
                serde_json::json!({
                    "kind": "local",
                    "raw": raw,
                }),
            ))
        }
        VmValue::String(text) => {
            if let Some(target) = text.strip_prefix("a2a://") {
                return Ok((
                    TriggerHandlerSpec::A2a {
                        target: target.to_string(),
                        allow_cleartext,
                    },
                    serde_json::json!({
                        "kind": "a2a",
                        "target": target,
                        "allow_cleartext": allow_cleartext,
                    }),
                ));
            }
            if let Some(queue) = text.strip_prefix("worker://") {
                return Ok((
                    TriggerHandlerSpec::Worker {
                        queue: queue.to_string(),
                    },
                    serde_json::json!({
                        "kind": "worker",
                        "queue": queue,
                    }),
                ));
            }
            Err(VmError::Runtime(format!(
                "{builtin}: `{field_name}` string must use `a2a://` or `worker://` URI syntax"
            )))
        }
        VmValue::Dict(map) => parse_handler_dict(map, builtin, field_name),
        other => Err(VmError::Runtime(format!(
            "{builtin}: `{field_name}` must be a closure, handler URI string, or handler-variant dict, got {}",
            other.type_name()
        ))),
    }
}

/// Parse handler-variant dicts. `SpawnToPool` (#1889) and `ReminderInject`
/// (#1876) both ship as dict-shaped handlers so the trigger DSL keeps a
/// single uniform `handler:` syntax; future variants plug in here too.
fn parse_handler_dict(
    map: &crate::value::DictMap,
    builtin: &str,
    field_name: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let kind = map
        .get("kind")
        .or_else(|| map.get("_kind"))
        .map(VmValue::display)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: `{field_name}` handler dict missing `kind`"
            ))
        })?;
    match kind.as_str() {
        "spawn_to_pool" => {
            let pool = map
                .get("pool")
                .map(VmValue::display)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{builtin}: SpawnToPool handler requires `pool` (string)"
                    ))
                })?;
            let priority_from = optional_path_string(map, "priority_from", "SpawnToPool")?;
            let key_from = optional_path_string(map, "key_from", "SpawnToPool")?;
            let task_factory = map
                .get("task_factory")
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{builtin}: SpawnToPool handler requires `task_factory` closure"
                    ))
                })
                .and_then(|value| match value {
                    VmValue::Closure(closure) => Ok(closure.clone()),
                    other => Err(VmError::Runtime(format!(
                        "{builtin}: SpawnToPool.task_factory must be a closure, got {}",
                        other.type_name()
                    ))),
                })?;
            let descriptor = serde_json::json!({
                "kind": "spawn_to_pool",
                "pool": pool,
                "priority_from": priority_from,
                "key_from": key_from,
                "task_factory": task_factory.func.name.clone(),
            });
            Ok((
                TriggerHandlerSpec::SpawnToPool {
                    pool,
                    priority_from,
                    key_from,
                    task_factory,
                },
                descriptor,
            ))
        }
        "reminder_inject" => parse_reminder_inject_handler(map, builtin),
        "interrupt_and_suspend" => parse_interrupt_and_suspend_handler(map, builtin),
        other => Err(VmError::Runtime(format!(
            "{builtin}: unsupported handler variant '{other}'; expected 'spawn_to_pool', 'reminder_inject', or 'interrupt_and_suspend'"
        ))),
    }
}

/// CH-10 (#1910): parse the `InterruptAndSuspend` handler dict. `target_agents`
/// resolution is one of: the string `"all"` (panic-broadcast every running
/// worker in the local registry), a list of concrete worker-id strings, or a
/// closure that returns a worker-id list at dispatch time (the closure form
/// lets a single trigger registration pick targets dynamically — e.g. all
/// workers tagged with a given org / tenant). `reason` is propagated to every
/// suspended worker's `WorkerSuspension::reason` and audit entry.
fn parse_interrupt_and_suspend_handler(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let target_value = map.get("target_agents").or_else(|| map.get("target"));
    let target_agents = match target_value {
        None | Some(VmValue::Nil) => AgentScope::All,
        Some(VmValue::String(s)) => match s.as_str() {
            "all" | "all_in_scope" => AgentScope::All,
            other if !other.is_empty() => AgentScope::Concrete(vec![other.to_string()]),
            _ => {
                return Err(VmError::Runtime(format!(
                    "{builtin}: InterruptAndSuspend `target_agents` string must be 'all' or a non-empty worker id"
                )));
            }
        },
        Some(VmValue::List(items)) => {
            let mut ids = Vec::with_capacity(items.len());
            for item in items.iter() {
                let VmValue::String(text) = item else {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: InterruptAndSuspend `target_agents` list entries must be strings, got {}",
                        item.type_name()
                    )));
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: InterruptAndSuspend `target_agents` list entries must be non-empty strings"
                    )));
                }
                ids.push(trimmed.to_string());
            }
            AgentScope::Concrete(ids)
        }
        Some(VmValue::Closure(closure)) => AgentScope::Closure(closure.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: InterruptAndSuspend `target_agents` must be the string 'all', a list of worker-id strings, or a closure returning a list, got {}",
                other.type_name()
            )));
        }
    };

    let reason = match map.get("reason") {
        Some(VmValue::String(s)) => s.to_string(),
        None | Some(VmValue::Nil) => "panic".to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: InterruptAndSuspend `reason` must be a string, got {}",
                other.type_name()
            )));
        }
    };

    let scope_kind = target_agents.kind();
    let concrete_count = match &target_agents {
        AgentScope::Concrete(ids) => Some(ids.len()),
        _ => None,
    };
    let descriptor = serde_json::json!({
        "kind": "interrupt_and_suspend",
        "scope_kind": scope_kind,
        "concrete_count": concrete_count,
        "reason": &reason,
    });

    Ok((
        TriggerHandlerSpec::InterruptAndSuspend {
            target_agents,
            reason,
        },
        descriptor,
    ))
}

/// Parse the ReminderInject (#1876) handler dict. `target` resolution is
/// one of: a string literal (`"current"`, `"parent"`, or any other value
/// interpreted as a concrete session id), or a closure that returns the
/// session id at dispatch time. Reminder metadata mirrors the
/// `transcript.inject_reminder` (#1815 R-02) shape so authors can reuse
/// the same mental model.
fn parse_reminder_inject_handler(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<(TriggerHandlerSpec, serde_json::Value), VmError> {
    let target_value = map.get("target").or_else(|| map.get("target_session_id"));
    let target = match target_value {
        None | Some(VmValue::Nil) => TargetExpr::Current,
        Some(VmValue::String(s)) => match s.as_str() {
            "current" => TargetExpr::Current,
            "parent" => TargetExpr::Parent,
            other if !other.is_empty() => TargetExpr::Concrete(other.to_string()),
            _ => {
                return Err(VmError::Runtime(format!(
                    "{builtin}: ReminderInject `target` string must be 'current', 'parent', or a non-empty session id"
                )));
            }
        },
        Some(VmValue::Closure(closure)) => TargetExpr::Closure(closure.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `target` must be a string ('current'/'parent'/session id) or a closure returning a session id, got {}",
                other.type_name()
            )));
        }
    };

    let body = match map.get("body") {
        Some(VmValue::String(s)) => s.to_string(),
        None | Some(VmValue::Nil) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject handler requires `body` (string template)"
            )));
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `body` must be a string, got {}",
                other.type_name()
            )));
        }
    };

    let tags = parse_reminder_tags(map, builtin)?;
    let ttl_turns = parse_reminder_ttl_turns(map, builtin)?;
    let dedupe_key = optional_path_string(map, "dedupe_key", "ReminderInject")?;
    let propagate = parse_reminder_propagate(map, builtin)?;
    let role_hint = parse_reminder_role_hint(map, builtin)?;
    let preserve_on_compact = match map.get("preserve_on_compact") {
        None | Some(VmValue::Nil) => false,
        Some(VmValue::Bool(b)) => *b,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `preserve_on_compact` must be a bool, got {}",
                other.type_name()
            )));
        }
    };

    let descriptor = serde_json::json!({
        "kind": "reminder_inject",
        "target": target.kind(),
        "target_session_id": match &target {
            TargetExpr::Concrete(id) => serde_json::Value::String(id.clone()),
            _ => serde_json::Value::Null,
        },
        "body": &body,
        "tags": &tags,
        "ttl_turns": ttl_turns,
        "dedupe_key": &dedupe_key,
        "propagate": propagate.as_str(),
        "role_hint": role_hint.as_str(),
        "preserve_on_compact": preserve_on_compact,
    });

    Ok((
        TriggerHandlerSpec::ReminderInject {
            target,
            body,
            tags,
            ttl_turns,
            dedupe_key,
            propagate,
            role_hint,
            preserve_on_compact,
        },
        descriptor,
    ))
}

fn parse_reminder_tags(map: &crate::value::DictMap, builtin: &str) -> Result<Vec<String>, VmError> {
    match map.get("tags") {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(list)) => {
            let mut tags = Vec::with_capacity(list.len());
            for item in list.iter() {
                let VmValue::String(text) = item else {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: ReminderInject `tags` entries must be strings, got {}",
                        item.type_name()
                    )));
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(VmError::Runtime(format!(
                        "{builtin}: ReminderInject `tags` entries must be non-empty strings"
                    )));
                }
                let tag = trimmed.to_string();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            Ok(tags)
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `tags` must be a list of strings, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_ttl_turns(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match map.get("ttl_turns") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) => {
            if *value <= 0 {
                Err(VmError::Runtime(format!(
                    "{builtin}: ReminderInject `ttl_turns` must be > 0"
                )))
            } else {
                Ok(Some(*value))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `ttl_turns` must be an int or nil, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_propagate(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<crate::llm::helpers::ReminderPropagate, VmError> {
    match map.get("propagate") {
        None | Some(VmValue::Nil) => Ok(crate::llm::helpers::ReminderPropagate::Session),
        Some(VmValue::String(s)) => match s.as_str() {
            "all" => Ok(crate::llm::helpers::ReminderPropagate::All),
            "session" => Ok(crate::llm::helpers::ReminderPropagate::Session),
            "none" => Ok(crate::llm::helpers::ReminderPropagate::None),
            other => Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `propagate` must be 'all', 'session', or 'none', got '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `propagate` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_reminder_role_hint(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<crate::llm::helpers::ReminderRoleHint, VmError> {
    match map.get("role_hint") {
        None | Some(VmValue::Nil) => Ok(crate::llm::helpers::ReminderRoleHint::System),
        Some(VmValue::String(s)) => match s.as_str() {
            "system" => Ok(crate::llm::helpers::ReminderRoleHint::System),
            "developer" => Ok(crate::llm::helpers::ReminderRoleHint::Developer),
            "user_block" => Ok(crate::llm::helpers::ReminderRoleHint::UserBlock),
            "ephemeral_cache" => Ok(crate::llm::helpers::ReminderRoleHint::EphemeralCache),
            other => Err(VmError::Runtime(format!(
                "{builtin}: ReminderInject `role_hint` must be 'system', 'developer', 'user_block', or 'ephemeral_cache', got '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: ReminderInject `role_hint` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_path_string(
    map: &crate::value::DictMap,
    field: &str,
    variant: &str,
) -> Result<Option<String>, VmError> {
    match map.get(field) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{variant}: `{field}` must be a string path or nil, got {}",
            other.type_name()
        ))),
    }
}
