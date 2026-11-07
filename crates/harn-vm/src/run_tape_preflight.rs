//! Read-only effect planning through the same policy the branch executor uses.

use super::prefix::{plan_call, PrefixPlan};
use crate::value::VmValue;
use harn_builtin_meta::CapabilityId;
use serde::Serialize;
use std::path::Path;

/// One startup invocation supplied by the host. Arguments retain their VM shape.
pub struct PrefixEffectCall<'a> {
    pub capability: Option<CapabilityId>,
    pub name: &'a str,
    pub args: &'a [VmValue],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefixEffectAction {
    Run,
    RecordedRead,
    RecordedCommand,
    Refused,
}

#[derive(Debug, Serialize)]
pub struct PrefixEffectDecision {
    pub call_index: usize,
    pub capability: Option<&'static str>,
    pub name: String,
    pub action: PrefixEffectAction,
    pub reason: Option<String>,
}

/// Covers only the supplied invocations, not undiscovered calls in a program.
#[derive(Debug, Serialize)]
pub struct PrefixEffectPreflight {
    pub supplied_calls: usize,
    pub refused_calls: usize,
    pub decisions: Vec<PrefixEffectDecision>,
}

/// List every effect-policy refusal without executing calls or starting a branch.
/// This does not validate arguments or prove that recorded read/command payloads
/// exist. The host must supply its complete startup invocation inventory.
pub fn preflight_prefix_calls(
    workspace: &Path,
    calls: &[PrefixEffectCall<'_>],
) -> PrefixEffectPreflight {
    let decisions: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(call_index, call)| {
            let (owner, effects) = if let Some(capability) = call.capability {
                (
                    Some((capability, call.name)),
                    crate::harness_methods::capability_method_effects(capability, call.name),
                )
            } else {
                let recorded = crate::stdlib::recorded_effect_builtin_manifest_entry(call.name)
                    .map(|entry| entry.contract.effects);
                crate::native_vm::builtin_tape_contract(call.name, recorded)
            };
            let (action, reason) = match plan_call(owner, effects, call.args, workspace) {
                PrefixPlan::Run => (PrefixEffectAction::Run, None),
                PrefixPlan::Read => (PrefixEffectAction::RecordedRead, None),
                PrefixPlan::Command => (PrefixEffectAction::RecordedCommand, None),
                PrefixPlan::Refuse(reason) => (PrefixEffectAction::Refused, Some(reason)),
            };
            PrefixEffectDecision {
                call_index,
                capability: owner.map(|(capability, _)| capability.field_name()),
                name: call.name.to_owned(),
                action,
                reason,
            }
        })
        .collect();
    PrefixEffectPreflight {
        supplied_calls: calls.len(),
        refused_calls: decisions
            .iter()
            .filter(|decision| decision.action == PrefixEffectAction::Refused)
            .count(),
        decisions,
    }
}
