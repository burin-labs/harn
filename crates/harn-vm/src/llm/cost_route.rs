use crate::value::VmDictExt;

use crate::value::{VmError, VmValue};

tokio::task_local! {
    static COST_ROUTE_STACK: Vec<crate::value::DictMap>;
}

fn strategy_text(config: &crate::value::DictMap) -> Option<String> {
    config
        .get("fallback_strategy")
        .or_else(|| config.get("strategy"))
        .map(|value| value.display())
        .filter(|value| !value.trim().is_empty())
}

fn quality_text(config: &crate::value::DictMap) -> String {
    config
        .get("quality")
        .or_else(|| config.get("min_quality"))
        .map(|value| value.display())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "mid".to_string())
}

fn route_policy_dict(
    mode: &str,
    target: Option<String>,
    prefer: Option<VmValue>,
    strategy: Option<String>,
) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("mode", mode);
    if let Some(target) = target {
        dict.put_str("target", target);
    }
    if let Some(prefer) = prefer {
        dict.insert(crate::value::intern_key("prefer"), prefer);
    }
    if let Some(strategy) = strategy {
        dict.put_str("strategy", strategy);
    }
    VmValue::dict(dict)
}

fn merge_budget_aliases(options: &mut crate::value::DictMap) {
    let Some(budget_usd) = options.get("budget_usd").cloned() else {
        return;
    };
    let mut budget = match options.get("budget") {
        Some(VmValue::Dict(existing)) => existing.as_ref().clone(),
        _ => crate::value::DictMap::new(),
    };
    budget
        .entry(crate::value::intern_key("max_cost_usd"))
        .or_insert(budget_usd.clone());
    options
        .entry(crate::value::intern_key("max_cost_usd"))
        .or_insert(budget_usd);
    options.insert(crate::value::intern_key("budget"), VmValue::dict(budget));
}

fn normalize_config(mut config: crate::value::DictMap) -> crate::value::DictMap {
    merge_budget_aliases(&mut config);
    if config.contains_key("route_policy") {
        return config;
    }

    if let Some(prefer) = config.get("prefer").cloned() {
        let strategy = strategy_text(&config).unwrap_or_else(|| "prefer_order".to_string());
        config.insert(
            crate::value::intern_key("route_policy"),
            route_policy_dict("preference_list", None, Some(prefer), Some(strategy)),
        );
        return config;
    }

    if let Some(strategy) = strategy_text(&config) {
        let normalized = strategy.trim().to_ascii_lowercase();
        let mode = match normalized.as_str() {
            "cheapest_first" | "cheapest" => Some("cheapest_over_quality"),
            "fastest_first" | "fastest" => Some("fastest_over_quality"),
            _ => None,
        };
        if let Some(mode) = mode {
            config.insert(
                crate::value::intern_key("route_policy"),
                route_policy_dict(mode, Some(quality_text(&config)), None, None),
            );
        }
    }

    config
}

fn merge_budget(inherited: Option<&VmValue>, explicit: Option<&VmValue>) -> Option<VmValue> {
    let mut merged = match inherited {
        Some(VmValue::Dict(dict)) => dict.as_ref().clone(),
        Some(value) => {
            let mut dict = crate::value::DictMap::new();
            dict.insert(crate::value::intern_key("max_cost_usd"), value.clone());
            dict
        }
        None => crate::value::DictMap::new(),
    };
    if let Some(VmValue::Dict(dict)) = explicit {
        for (key, value) in dict.iter() {
            merged.insert(key.clone(), value.clone());
        }
    } else if let Some(value) = explicit {
        merged.insert(crate::value::intern_key("max_cost_usd"), value.clone());
    }
    (!merged.is_empty()).then(|| VmValue::dict(merged))
}

pub(crate) fn merge_context_options(
    explicit: Option<crate::value::DictMap>,
) -> Option<crate::value::DictMap> {
    let inherited = COST_ROUTE_STACK
        .try_with(|stack| {
            let mut merged = crate::value::DictMap::new();
            for frame in stack.iter() {
                for (key, value) in frame {
                    merged.insert(key.clone(), value.clone());
                }
            }
            merged
        })
        .unwrap_or_default();

    if inherited.is_empty() {
        return explicit;
    }

    let mut merged = inherited;
    if let Some(explicit) = explicit {
        let budget = merge_budget(merged.get("budget"), explicit.get("budget"));
        for (key, value) in explicit {
            merged.insert(key, value);
        }
        if let Some(budget) = budget {
            merged.insert(crate::value::intern_key("budget"), budget);
        }
    }
    Some(merged)
}

pub(crate) async fn cost_route_impl(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let config = match args.first().and_then(VmValue::as_dict) {
        Some(config) => normalize_config(config.clone()),
        None => {
            return Err(VmError::Runtime(
                "cost_route: first argument must be a config dict".to_string(),
            ))
        }
    };
    let closure = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(VmError::Runtime(
                "cost_route: second argument must be a closure".to_string(),
            ))
        }
    };

    let mut child_vm = ctx.child_vm();
    let mut stack = COST_ROUTE_STACK
        .try_with(|current| current.clone())
        .unwrap_or_default();
    stack.push(config);
    let result = COST_ROUTE_STACK
        .scope(stack, child_vm.call_closure_pub(&closure, &[]))
        .await;
    ctx.forward_output(&child_vm.take_output());
    result
}
