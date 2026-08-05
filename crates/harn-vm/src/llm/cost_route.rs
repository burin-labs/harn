use crate::value::{VmError, VmValue};

tokio::task_local! {
    static COST_ROUTE_STACK: Vec<crate::value::DictMap>;
}

fn normalize_config(config: crate::value::DictMap) -> Result<crate::value::DictMap, VmError> {
    crate::llm::helpers::validate_llm_option_keys(&config)?;
    Ok(config)
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
        Some(config) => normalize_config(config.clone())?,
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
