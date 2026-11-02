//! Native lifecycle policy around annotated interpreter calls.

use crate::{
    orchestration::{self, HookEvent},
    step_runtime,
    value::{VmClosure, VmDictExt, VmError, VmValue},
    vm::{step_context::ActiveStep, Vm},
};
use std::sync::Arc;

fn domain_args(args: &[VmValue]) -> &[VmValue] {
    match args.first() {
        Some(VmValue::Harness(handle)) if handle.kind() == crate::harness::HarnessKind::Root => {
            &args[1..]
        }
        _ => args,
    }
}

fn payload(
    event: HookEvent,
    persona: Option<&str>,
    definition: &crate::vm::step_context::StepDefinition,
    args: &[VmValue],
    output: Option<VmValue>,
) -> VmValue {
    let mut step = std::collections::BTreeMap::new();
    step.put_str("name", &definition.name);
    step.put_str("function", &definition.function);
    step.put("args", VmValue::list(domain_args(args).to_vec()));
    let mut result = std::collections::BTreeMap::new();
    result.put_str("event", event.as_str());
    result.put_str(
        "target",
        match persona {
            Some(persona) if !persona.is_empty() => format!("{persona}.{}", definition.name),
            _ => definition.name.clone(),
        },
    );
    result.put_str("persona", persona.unwrap_or(""));
    result.put("step", VmValue::dict(step));
    if let Some(output) = output {
        result.insert("output".to_string(), output);
    }
    VmValue::dict(result)
}

fn denied(step_name: &str, reason: String) -> VmError {
    let mut error = std::collections::BTreeMap::new();
    error.put_str("category", "hook_denied");
    error.put_str("event", "PreStep");
    error.put_str("reason", reason);
    error.put_str("step", step_name);
    VmError::Thrown(VmValue::dict(error))
}

fn pre_result(
    value: VmValue,
    current_args: Vec<VmValue>,
    step_name: &str,
) -> Result<Vec<VmValue>, VmError> {
    match value {
        VmValue::Nil => Ok(current_args),
        VmValue::String(text) if text.as_str() == "Allow" => Ok(current_args),
        VmValue::Dict(map) => {
            if let Some(reason) = map.get("deny").or_else(|| map.get("reason")) {
                return Err(denied(step_name, reason.display()));
            }
            if matches!(
                map.get("action").map(|value| value.display()).as_deref(),
                Some("deny" | "Deny")
            ) {
                return Err(denied(
                    step_name,
                    map.get("reason")
                        .map(|value| value.display())
                        .unwrap_or_else(|| "step hook denied execution".to_string()),
                ));
            }
            if let Some(VmValue::List(args)) = map.get("args").or_else(|| map.get("modify")) {
                let has_harness = domain_args(&current_args).len() < current_args.len();
                let mut modified = Vec::with_capacity(args.len() + usize::from(has_harness));
                if has_harness {
                    modified.push(current_args[0].clone());
                }
                modified.extend((**args).clone());
                return Ok(modified);
            }
            Ok(current_args)
        }
        other => Err(VmError::Runtime(format!(
            "PreStep hook must return nil, Allow, or {{deny|args}}, got {}",
            other.type_name()
        ))),
    }
}

fn post_result(value: VmValue, current_output: VmValue) -> Result<VmValue, VmError> {
    match value {
        VmValue::Nil => Ok(current_output),
        VmValue::String(text) if text.as_str() == "Pass" => Ok(current_output),
        VmValue::Dict(map) => Ok(map
            .get("output")
            .or_else(|| map.get("result"))
            .or_else(|| map.get("modify"))
            .cloned()
            .unwrap_or(current_output)),
        other => Err(VmError::Runtime(format!(
            "PostStep hook must return nil, Pass, or {{output}}, got {}",
            other.type_name()
        ))),
    }
}

async fn invoke(
    vm: &mut Vm,
    handler: &Arc<VmClosure>,
    event: HookEvent,
    payload: VmValue,
) -> Result<VmValue, VmError> {
    let active_context = step_runtime::suspend_active_context();
    let result = vm.call_closure_pub(handler, &[payload]).await;
    drop(active_context);
    let (action, effects) =
        orchestration::collect_hook_effects_and_action(event, result?, VmValue::Nil)?;
    orchestration::inject_hook_effects_into_current_session(effects)?;
    Ok(action)
}

pub(super) async fn before(
    vm: &mut Vm,
    closure: &VmClosure,
    mut args: Vec<VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    let Some(definition) = step_runtime::step_definition_for_function(&closure.func.name) else {
        return Ok(args);
    };
    let persona = step_runtime::current_persona_name();
    let initial = payload(
        HookEvent::PreStep,
        persona.as_deref(),
        &definition,
        &args,
        None,
    );
    let manifest_hooks = orchestration::matching_vm_lifecycle_hooks(
        HookEvent::PreStep,
        &crate::value::json::vm_value_to_json(&initial),
    );
    for hook in manifest_hooks {
        let next = payload(
            HookEvent::PreStep,
            persona.as_deref(),
            &definition,
            &args,
            None,
        );
        let handler = hook.resolve(vm).await?;
        let raw = hook
            .scope(invoke(vm, &handler, HookEvent::PreStep, next))
            .await?;
        args = pre_result(raw, args, &definition.name)?;
    }
    // Manifest handlers may register more hooks, so select these afterward.
    let hooks = step_runtime::matching_hooks(
        HookEvent::PreStep,
        persona.as_deref(),
        Some(&definition.name),
        None,
    );
    for hook in hooks {
        let next = payload(hook.event, persona.as_deref(), &definition, &args, None);
        let raw = invoke(vm, &hook.handler, HookEvent::PreStep, next).await?;
        args = pre_result(raw, args, &definition.name)?;
    }
    Ok(args)
}

pub(super) async fn finish(
    vm: &mut Vm,
    step: ActiveStep,
    output: VmValue,
) -> Result<VmValue, VmError> {
    let persona = step.persona.as_deref();
    let hooks = step_runtime::matching_hooks(
        HookEvent::PostStep,
        persona,
        Some(&step.definition.name),
        None,
    );
    let result = async {
        let mut current = output;
        let initial = payload(
            HookEvent::PostStep,
            persona,
            &step.definition,
            &step.args,
            Some(current.clone()),
        );
        let manifest_hooks = orchestration::matching_vm_lifecycle_hooks(
            HookEvent::PostStep,
            &crate::value::json::vm_value_to_json(&initial),
        );
        for hook in manifest_hooks {
            let next = payload(
                HookEvent::PostStep,
                persona,
                &step.definition,
                &step.args,
                Some(current.clone()),
            );
            let handler = hook.resolve(vm).await?;
            current = post_result(
                hook.scope(invoke(vm, &handler, HookEvent::PostStep, next))
                    .await?,
                current,
            )?;
        }
        for hook in hooks {
            let next = payload(
                hook.event,
                persona,
                &step.definition,
                &step.args,
                Some(current.clone()),
            );
            current = post_result(
                invoke(vm, &hook.handler, HookEvent::PostStep, next).await?,
                current,
            )?;
        }
        Ok::<VmValue, VmError>(current)
    }
    .await;
    match result {
        Ok(value) => {
            step_runtime::finish_active_step(step, "completed", None);
            Ok(value)
        }
        Err(error) => {
            step_runtime::finish_active_step(step, "failed", Some(error.to_string()));
            Err(error)
        }
    }
}
