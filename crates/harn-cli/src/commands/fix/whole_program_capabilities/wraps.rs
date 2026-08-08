//! Wrap value-referenced callables so a capability parameter can be introduced
//! without silently shifting the invisible `handler(args)` dispatch.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};

use harn_parser::DiagnosticCode as Code;

use super::super::value_wrap::wrap_value_reference_edit;
use super::ambient_call_capability;
use super::edits::argument_for_kind;
use super::{CarrierKind, FrozenCallable, FrozenCause, ProgramCallable, ProgramFile};

/// Seed ambient requirements from AST ambient calls when per-file synthesis
/// dropped a frozen owner's diagnostics. Host/manifest entries stay untouched.
pub(super) fn seed_frozen_owner_ambient_requirements(callables: &mut [ProgramCallable]) {
    for callable in callables.iter_mut() {
        if callable.info.is_host_entry
            || matches!(
                callable.info.frozen_cause,
                Some(FrozenCause::HostEntry | FrozenCause::ManifestHandler)
            )
        {
            continue;
        }
        if !callable.direct_requirements.is_empty() || callable.direct_root_requirement {
            continue;
        }
        for call in &callable.info.ambient_capability_calls {
            if let Some(capability) = ambient_call_capability(call) {
                callable.direct_requirements.insert(capability);
            } else if call.code == Code::LintAmbientHarnessMethod {
                callable.direct_root_requirement = true;
            }
        }
    }
}

/// Decide which value-referenced callables can be unblocked by a wrap.
///
/// Containers that must supply the capability into a wrap get
/// `root_requirements` set so the subsequent propagate/desired pass threads
/// them. Wrap text is emitted later in [`emit_value_reference_wraps`] once
/// desired carriers are known.
pub(super) fn mark_value_reference_wraps(
    program_files: &[ProgramFile],
    callables: &[ProgramCallable],
    arity_observable: &[bool],
    requirements: &[BTreeSet<CapabilityId>],
    root_requirements: &mut [bool],
    frozen: &mut Vec<FrozenCallable>,
) -> Vec<bool> {
    let mut wrappable = vec![false; callables.len()];
    let callable_by_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| (callable.info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();
    let mut sites_by_name: BTreeMap<&str, Vec<(usize, Span)>> = BTreeMap::new();
    for (file_idx, file) in program_files.iter().enumerate() {
        for site in &file.value_reference_sites {
            if callable_by_name.contains_key(site.name.as_str()) {
                sites_by_name
                    .entry(site.name.as_str())
                    .or_default()
                    .push((file_idx, site.span));
            }
        }
    }

    for (idx, callable) in callables.iter().enumerate() {
        if !arity_observable[idx] {
            continue;
        }
        let needs_new_param = callable.carrier.is_none()
            && (!requirements[idx].is_empty()
                || root_requirements[idx]
                || !callable.info.ambient_capability_calls.is_empty());
        if !needs_new_param {
            continue;
        }

        let sites = sites_by_name
            .get(callable.info.name.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let site_locations = || {
            sites
                .iter()
                .map(|(file_idx, span)| {
                    (
                        program_files[*file_idx].path.display().to_string(),
                        span.line,
                    )
                })
                .collect::<Vec<_>>()
        };

        // Host and manifest entries fix arity outside this program; wraps
        // cannot invent an argument the embedding runtime was never asked to
        // supply (#6193 / #6272).
        if let Some(cause @ (FrozenCause::HostEntry | FrozenCause::ManifestHandler)) =
            callable.info.frozen_cause
        {
            record_frozen_callable(frozen, &callable.info.name, cause, &site_locations());
            continue;
        }
        if sites.is_empty() {
            record_frozen_callable(
                frozen,
                &callable.info.name,
                FrozenCause::ValueReference,
                &[],
            );
            continue;
        }

        let mut container_idxs = Vec::new();
        let mut refused = false;
        for &(file_idx, span) in sites {
            let Some(container_idx) = containing_callable(callables, file_idx, span) else {
                refused = true;
                break;
            };
            if matches!(
                callables[container_idx].info.frozen_cause,
                Some(FrozenCause::HostEntry | FrozenCause::ManifestHandler)
            ) || callables[container_idx].info.is_host_entry
            {
                refused = true;
                break;
            }
            if wrap_value_reference_edit(
                &program_files[file_idx].source,
                span,
                &callable.info.name,
                &callable.info.param_names,
            )
            .is_none()
            {
                refused = true;
                break;
            }
            container_idxs.push(container_idx);
        }
        if refused {
            record_frozen_callable(
                frozen,
                &callable.info.name,
                FrozenCause::ValueReference,
                &site_locations(),
            );
            continue;
        }

        for container_idx in container_idxs {
            root_requirements[container_idx] = true;
        }
        wrappable[idx] = true;
    }

    wrappable
}

pub(super) fn emit_value_reference_wraps(
    program_files: &[ProgramFile],
    callables: &[ProgramCallable],
    wrappable: &[bool],
    desired: &[Option<CarrierKind>],
    added_capabilities: &[BTreeMap<CapabilityId, String>],
) -> Result<BTreeMap<usize, Vec<FixEdit>>, String> {
    let mut edits_by_file: BTreeMap<usize, Vec<FixEdit>> = BTreeMap::new();
    let callable_by_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| (callable.info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();

    for (file_idx, file) in program_files.iter().enumerate() {
        for site in &file.value_reference_sites {
            let Some(&callee_idx) = callable_by_name.get(site.name.as_str()) else {
                continue;
            };
            if !wrappable[callee_idx] {
                continue;
            }
            let Some(container_idx) = containing_callable(callables, file_idx, site.span) else {
                continue;
            };
            let callee = &callables[callee_idx];
            let caller = &callables[container_idx];
            let Some(caller_desired) = desired[container_idx].as_ref() else {
                return Err(format!(
                    "wrap for `{}` needs a capability in `{}`, but none was planned",
                    callee.info.name, caller.info.name
                ));
            };
            let Some(callee_desired) = desired[callee_idx].as_ref() else {
                return Err(format!(
                    "wrap for `{}` planned without a desired carrier",
                    callee.info.name
                ));
            };
            let argument = argument_for_kind(
                caller,
                caller_desired,
                &added_capabilities[container_idx],
                callee_desired,
            )
            .map_err(|error| {
                format!(
                    "wrap for `{}` inside `{}`: {error}",
                    callee.info.name, caller.info.name
                )
            })?;
            let params = callee.info.param_names.join(", ");
            let call_args = if params.is_empty() {
                argument
            } else {
                format!("{argument}, {params}")
            };
            let replacement = if params.is_empty() {
                format!("{{ -> {}({call_args}) }}", callee.info.name)
            } else {
                format!("{{ {params} -> {}({call_args}) }}", callee.info.name)
            };
            let region = file
                .source
                .get(site.span.start..site.span.end)
                .ok_or_else(|| {
                    format!(
                        "wrap site for `{}` is out of range in {}",
                        callee.info.name,
                        file.path.display()
                    )
                })?;
            if region != callee.info.name {
                return Err(format!(
                    "wrap site for `{}` in {} no longer names the callable",
                    callee.info.name,
                    file.path.display()
                ));
            }
            edits_by_file.entry(file_idx).or_default().push(FixEdit {
                span: site.span,
                replacement,
            });
        }
    }
    Ok(edits_by_file)
}

fn containing_callable(
    callables: &[ProgramCallable],
    file_idx: usize,
    span: Span,
) -> Option<usize> {
    callables
        .iter()
        .enumerate()
        .filter(|(_, callable)| {
            callable.file_idx == file_idx
                && callable.info.span.start <= span.start
                && callable.info.span.end >= span.end
        })
        .min_by_key(|(_, callable)| {
            callable
                .info
                .span
                .end
                .saturating_sub(callable.info.span.start)
        })
        .map(|(idx, _)| idx)
}

fn record_frozen_callable(
    frozen: &mut Vec<FrozenCallable>,
    name: &str,
    cause: FrozenCause,
    sites: &[(String, usize)],
) {
    if let Some(existing) = frozen.iter_mut().find(|entry| entry.name == name) {
        // Per-file synthesis may have recorded the freeze without sites; prefer
        // the whole-program reason that names the hand-over locations.
        if !sites.is_empty() && !existing.reason.contains("escaping reference") {
            *existing = FrozenCallable::new(name, cause, sites);
        }
        return;
    }
    frozen.push(FrozenCallable::new(name, cause, sites));
}
