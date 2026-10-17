//! Authoritative linked builtin definitions and their contract projections.

use super::builtin_def::{VmBuiltinDef, ALL_BUILTIN_DEFS};

pub fn all_builtin_defs() -> &'static [&'static VmBuiltinDef] {
    let defs = &ALL_BUILTIN_DEFS;
    validate_builtin_contracts(defs);
    defs
}

fn validate_builtin_contracts(defs: &[&VmBuiltinDef]) {
    use harn_builtin_meta::BuiltinExposure;
    let mut source_names = std::collections::BTreeSet::new();
    let mut capability_methods = std::collections::BTreeSet::new();
    for def in defs {
        assert!(
            def.contract.is_declared(),
            "builtin `{}` has no typed exposure/effect contract",
            def.sig.name
        );
        assert!(
            !matches!(def.contract.exposure, BuiltinExposure::PureGlobal)
                || def.contract.effects.is_empty(),
            "ambient global builtin `{}` declares effects; effects must flow through Harness",
            def.sig.name
        );
        if let BuiltinExposure::CapabilityFunction { authority_argument } = def.contract.exposure {
            assert!(
                !def.contract.effects.is_empty(),
                "capability function `{}` must declare effects",
                def.sig.name
            );
            assert!(
                usize::from(authority_argument) < def.sig.params.len(),
                "capability function `{}` authority argument is out of range",
                def.sig.name
            );
        }
        if def.runtime_only {
            assert!(
                matches!(
                    def.contract.exposure,
                    BuiltinExposure::StdlibInternal | BuiltinExposure::RuntimeInternal
                ),
                "runtime_only builtin `{}` must use runtime_internal exposure",
                def.sig.name
            );
        }
        if let BuiltinExposure::HarnessMethod { method, .. } = def.contract.exposure {
            assert!(
                !method.is_empty(),
                "harness method for `{}` cannot be empty",
                def.sig.name
            );
            let BuiltinExposure::HarnessMethod { capability, method } = def.contract.exposure
            else {
                unreachable!()
            };
            assert!(
                capability_methods.insert((capability, method)),
                "duplicate contract for harness.{}.{}",
                capability.field_name(),
                method
            );
        }
        if matches!(
            def.contract.exposure,
            BuiltinExposure::PureGlobal
                | BuiltinExposure::CapabilityFunction { .. }
                | BuiltinExposure::PrivilegedWire
                | BuiltinExposure::StdlibInternal
                | BuiltinExposure::HarnessMethod { .. }
        ) {
            assert!(
                source_names.insert(def.sig.name),
                "duplicate source contract name `{}`",
                def.sig.name
            );
        }
    }
}

/// Driver-facing immutable manifest for parser, IR, policy, and docs.
pub fn all_builtin_manifest() -> &'static [&'static harn_builtin_registry::BuiltinManifestEntry] {
    use std::sync::OnceLock;
    static AGG: OnceLock<Vec<&'static harn_builtin_registry::BuiltinManifestEntry>> =
        OnceLock::new();
    AGG.get_or_init(|| {
        let mut out = Vec::new();
        let mut capability_methods = std::collections::BTreeSet::new();
        for def in all_builtin_defs() {
            if def.runtime_only {
                continue;
            }
            out.push(
                Box::leak(Box::new(harn_builtin_registry::BuiltinManifestEntry {
                    name: def.sig.name,
                    canonical_name: def.sig.name,
                    signature: &def.sig,
                    contract: def.contract,
                })) as &'static harn_builtin_registry::BuiltinManifestEntry,
            );
            if let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } =
                def.contract.exposure
            {
                capability_methods.insert((capability, method));
            }
            for alias in def.aliases {
                let signature = Box::leak(Box::new(harn_builtin_meta::BuiltinSignature {
                    name: alias,
                    ..def.sig
                }));
                out.push(
                    Box::leak(Box::new(harn_builtin_registry::BuiltinManifestEntry {
                        name: alias,
                        canonical_name: def.sig.name,
                        signature,
                        contract: def.contract,
                    })) as &'static harn_builtin_registry::BuiltinManifestEntry,
                );
            }
        }
        for entry in harn_capability_contracts::manifest() {
            let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } =
                entry.contract.exposure
            else {
                unreachable!("leaf capability manifest contains a non-method contract")
            };
            if !capability_methods.insert((capability, method)) {
                let runtime_entry = out
                    .iter()
                    .find(|candidate| candidate.contract.exposure == entry.contract.exposure)
                    .expect("duplicate capability key must have a runtime manifest entry");
                assert_eq!(
                    runtime_entry.contract,
                    entry.contract,
                    "runtime effect contract drift for harness.{}.{}",
                    capability.field_name(),
                    method
                );
                assert_eq!(
                    runtime_entry.signature,
                    entry.signature,
                    "runtime signature drift for harness.{}.{}",
                    capability.field_name(),
                    method
                );
                continue;
            }
            out.push(*entry);
        }
        for group in harn_builtin_meta::host_capabilities::all_host_capability_groups() {
            for method in group.methods {
                if !capability_methods.insert((group.capability, *method)) {
                    continue;
                }
                let internal_name: &'static str = Box::leak(
                    format!("__cap_{}_{}", group.capability.field_name(), method).into_boxed_str(),
                );
                let params: &'static [harn_builtin_meta::Param] =
                    Box::leak(Box::new([harn_builtin_meta::Param::new(
                        "request",
                        harn_builtin_meta::Ty::Named("dict"),
                    )]));
                let signature = Box::leak(Box::new(harn_builtin_meta::BuiltinSignature::simple(
                    internal_name,
                    params,
                    harn_builtin_meta::Ty::Named("dict"),
                )));
                out.push(Box::leak(Box::new(
                    harn_builtin_registry::BuiltinManifestEntry {
                        name: internal_name,
                        canonical_name: internal_name,
                        signature,
                        contract: harn_builtin_meta::BuiltinContract::harness(
                            group.capability,
                            method,
                            group.effects,
                        ),
                    },
                )));
            }
        }
        out
    })
    .as_slice()
}

/// Indexed view of the authoritative builtin manifest.
///
/// Runtime dispatch reaches this boundary for every typed Harness call. Keep
/// lookup policy here with the manifest owner instead of making each consumer
/// linearly rescan the full registry. The nested capability map also accepts a
/// borrowed `&str`, so a method call does not allocate an owned lookup key.
struct BuiltinManifestIndex {
    runtime_only_contracts:
        std::collections::HashMap<&'static str, &'static harn_builtin_meta::BuiltinContract>,
    by_name: std::collections::HashMap<
        &'static str,
        &'static harn_builtin_registry::BuiltinManifestEntry,
    >,
    by_capability: std::collections::HashMap<
        harn_builtin_meta::CapabilityId,
        std::collections::HashMap<
            &'static str,
            &'static harn_builtin_registry::BuiltinManifestEntry,
        >,
    >,
    recorded_effects_by_name: std::collections::HashMap<
        &'static str,
        &'static harn_builtin_registry::BuiltinManifestEntry,
    >,
}

fn builtin_manifest_index() -> &'static BuiltinManifestIndex {
    use harn_builtin_meta::BuiltinExposure;
    use std::sync::OnceLock;

    static INDEX: OnceLock<BuiltinManifestIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut by_name = std::collections::HashMap::new();
        let mut by_capability: std::collections::HashMap<
            harn_builtin_meta::CapabilityId,
            std::collections::HashMap<
                &'static str,
                &'static harn_builtin_registry::BuiltinManifestEntry,
            >,
        > = std::collections::HashMap::new();
        let mut recorded_effects_by_name = std::collections::HashMap::new();
        for entry in all_builtin_manifest() {
            assert!(
                by_name.insert(entry.name, *entry).is_none(),
                "duplicate builtin manifest name `{}`",
                entry.name
            );
            // An alias repeats its primary's contract under a second name, so
            // only the canonical entry may claim the capability method.
            if let BuiltinExposure::HarnessMethod { capability, method } = entry.contract.exposure {
                if entry.is_canonical() {
                    assert!(
                        by_capability
                            .entry(capability)
                            .or_default()
                            .insert(method, *entry)
                            .is_none(),
                        "duplicate Harness method manifest entry `harness.{}.{method}`",
                        capability.field_name()
                    );
                }
            }
            if matches!(
                entry.contract.exposure,
                BuiltinExposure::CapabilityFunction { .. }
                    | BuiltinExposure::HarnessMethod { .. }
                    | BuiltinExposure::PrivilegedWire
                    | BuiltinExposure::StdlibInternal
            ) && !entry.contract.effects.is_empty()
            {
                recorded_effects_by_name.insert(entry.name, *entry);
            }
        }
        let mut runtime_only_contracts = std::collections::HashMap::new();
        for def in all_builtin_defs().iter().filter(|def| def.runtime_only) {
            for name in std::iter::once(def.sig.name).chain(def.aliases.iter().copied()) {
                assert!(
                    !by_name.contains_key(name)
                        && runtime_only_contracts.insert(name, &def.contract).is_none(),
                    "duplicate runtime builtin contract `{name}`"
                );
            }
        }
        BuiltinManifestIndex {
            runtime_only_contracts,
            by_name,
            by_capability,
            recorded_effects_by_name,
        }
    })
}

/// Resolve the registry name behind a public `harness.<capability>.<method>`
/// path.
///
/// Observation, diagnostics, and tooling classify a capability call by the
/// same registry entry the removed ambient global resolved to, so a call
/// through the typed handle profiles and audits identically.
pub fn builtin_for_harness_path(path: &str) -> Option<&'static str> {
    let (field, method) = path.strip_prefix("harness.")?.split_once('.')?;
    let capability = harn_builtin_meta::CapabilityId::from_field_name(field)?;
    capability_method_manifest_entry(capability, method).map(|entry| entry.name)
}

/// Resolve the typed Harness method that replaced an ambient builtin name.
pub fn harness_method_for_builtin(
    name: &str,
) -> Option<(harn_builtin_meta::CapabilityId, &'static str)> {
    match builtin_manifest_entry(name)?.contract.exposure {
        harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, method } => {
            Some((capability, method))
        }
        _ => None,
    }
}

/// Resolve a source/runtime builtin contract without rescanning the manifest.
pub fn builtin_manifest_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index().by_name.get(name).copied()
}

/// Runtime-only constructors have contracts but are not source manifest entries.
pub fn builtin_policy_metadata(
    name: &str,
) -> (
    Option<&'static harn_builtin_registry::BuiltinManifestEntry>,
    Option<&'static harn_builtin_meta::BuiltinContract>,
) {
    let index = builtin_manifest_index();
    let entry = index.by_name.get(name).copied();
    let contract = entry
        .map(|entry| &entry.contract)
        .or_else(|| index.runtime_only_contracts.get(name).copied());
    (entry, contract)
}

/// Resolve the contract for one typed Harness method without allocation.
pub fn capability_method_manifest_entry(
    capability: harn_builtin_meta::CapabilityId,
    method: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index()
        .by_capability
        .get(&capability)
        .and_then(|methods| methods.get(method))
        .copied()
}

/// Resolve only builtin contracts that can emit runtime effect receipts.
///
/// Pure builtins dominate VM call volume. Keeping them out of this projection
/// makes their receipt check one negative hash lookup instead of a full
/// manifest lookup plus exposure classification on every call.
pub fn recorded_effect_builtin_manifest_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    builtin_manifest_index()
        .recorded_effects_by_name
        .get(name)
        .copied()
}
