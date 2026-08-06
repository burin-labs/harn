//! Public lookup helpers over the unified [`BuiltinSignature`] registry.
//!
//! Both the type checker (this crate) and the runtime (`harn-vm`) consume
//! these helpers. Generic builtins declare their type parameters via
//! [`BuiltinSignature::type_params`] and use [`Ty::Generic`]/[`Ty::SchemaOf`]
//! in param/return positions; the type checker materializes those at each
//! call site against the surrounding scope.

use crate::ast::TypeExpr;
use harn_builtin_meta::{BuiltinExposure, CapabilityId};

use super::signatures;
use super::{BuiltinMetadata, BuiltinSignature, Ty, TyExt};

/// Resolve the installed name index, then fall back to static signature groups.
/// Installed entries win when both sides carry the same name (the
/// `#[harn_builtin]`-emitted signature shadows any legacy static duplicate).
/// Runtime validation calls this for every builtin invocation, so the owning
/// registry lookup must not materialize or linearly scan its whole manifest.
pub fn lookup(name: &str) -> Option<&'static BuiltinSignature> {
    lookup_with_privileged_wire(name, false)
}

/// Resolve a builtin for an explicitly trusted host-dispatch compilation.
/// This widens only `PrivilegedWire`; it does not restore legacy Harness
/// methods or runtime-internal names as ambient globals.
pub fn lookup_with_privileged_wire(
    name: &str,
    allow_privileged_wire: bool,
) -> Option<&'static BuiltinSignature> {
    if let Some(entry) = harn_builtin_registry::builtin_entry(name) {
        return matches!(
            entry.contract.exposure,
            BuiltinExposure::PureGlobal | BuiltinExposure::CapabilityFunction { .. }
        )
        .then_some(entry.signature)
        .or_else(|| {
            (allow_privileged_wire && entry.contract.exposure == BuiltinExposure::PrivilegedWire)
                .then_some(entry.signature)
        })
        .or_else(|| {
            (crate::legacy_ambient_capabilities_enabled()
                && matches!(
                    entry.contract.exposure,
                    BuiltinExposure::HarnessMethod { .. }
                        | BuiltinExposure::PrivilegedWire
                        | BuiltinExposure::RuntimeInternal
                ))
            .then_some(entry.signature)
        });
    }
    if crate::legacy_ambient_capabilities_enabled() {
        if let Some(entry) = legacy_capability_method_entry(name) {
            return Some(entry.signature);
        }
        if let Some(entry) = legacy_ambient_cap_global_entry(name) {
            return Some(entry.signature);
        }
        if let Some(canonical) = crate::legacy_builtin_alias_target(name) {
            return lookup(canonical);
        }
    }
    for group in signatures::groups() {
        for sig in group {
            if sig.name == name {
                return Some(sig);
            }
        }
    }
    None
}

/// Resolve an unqualified legacy method name only when the typed manifest has
/// exactly one owning Harness capability. Ambiguous method spellings remain
/// unavailable rather than selecting authority by registration order.
pub fn legacy_capability_method_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    let mut matches = ambient_harness_method_entries()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.contract.exposure,
                BuiltinExposure::HarnessMethod { method, .. } if method == name
            )
        });
    let entry = matches.next()?;
    matches.next().is_none().then_some(entry)
}

/// Resolve a pre-cutover ambient global whose typed contract is published under
/// the hidden `__cap_<name>` spelling (for example `runtime_context_set` →
/// `__cap_runtime_context_set` for `harness.runtime.context_set`).
pub fn legacy_ambient_cap_global_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    ambient_harness_method_entries()
        .into_iter()
        .find(|entry| entry.name.strip_prefix("__cap_") == Some(name))
}

/// Resolve a privileged-wire builtin published as `__<name>` (for example
/// ambient `security_policy` → `__security_policy`).
pub fn legacy_privileged_wire_entry(
    name: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    harn_builtin_registry::installed_manifest()
        .into_iter()
        .find(|entry| {
            matches!(entry.contract.exposure, BuiltinExposure::PrivilegedWire)
                && entry.name.strip_prefix("__") == Some(name)
        })
}

/// Canonical runtime builtin name for an ambient call site under the legacy
/// bridge.
///
/// Only rewrite when the runtime registers a different spelling than the
/// source call. Privileged-wire builtins publish as `__name`. Host internals
/// (`__host_*`) and capability `__cap_*` contracts keep their short ambient
/// names; the VM projects those globals under the ambient bridge.
pub fn legacy_ambient_runtime_name(name: &str) -> Option<&'static str> {
    if let Some(target) = crate::legacy_builtin_alias_target(name) {
        return Some(target);
    }
    legacy_privileged_wire_entry(name).map(|entry| entry.name)
}

fn ambient_harness_method_entries() -> Vec<&'static harn_builtin_registry::BuiltinManifestEntry> {
    // Once the CLI/runtime installs the process manifest, prefer it alone.
    // Chaining the static capability-contracts table on top duplicates every
    // `__cap_*` method and makes `legacy_capability_method_entry` treat unique
    // owners as ambiguous (two identical matches), which breaks ambient check.
    let installed = harn_builtin_registry::installed_manifest();
    if installed.is_empty() {
        harn_capability_contracts::manifest()
            .iter()
            .copied()
            .collect()
    } else {
        installed
    }
}

/// Resolve the signature paired with one capability method contract.
pub fn lookup_capability_method(
    capability: CapabilityId,
    method: &str,
) -> Option<&'static BuiltinSignature> {
    capability_method_entry(capability.field_name(), method).map(|entry| entry.signature)
}

/// Resolve the single manifest entry that owns a `harness.<field>.<method>`
/// call. Consumers that need effects or the internal dispatch name use this
/// rather than reconstructing either from strings.
pub fn capability_method_entry(
    field: &str,
    method: &str,
) -> Option<&'static harn_builtin_registry::BuiltinManifestEntry> {
    let capability = CapabilityId::from_field_name(field)?;
    harn_builtin_registry::installed_manifest()
        .iter()
        .copied()
        .find(|entry| {
            matches!(
                entry.contract.exposure,
                BuiltinExposure::HarnessMethod {
                    capability: candidate,
                    method: candidate_method,
                } if candidate == capability && candidate_method == method
            )
        })
        .or_else(|| harn_capability_contracts::capability_method_entry(field, method))
}

/// Is `name` a builtin known to the parser?
pub fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
        || (crate::legacy_ambient_capabilities_enabled()
            && crate::is_registered_legacy_hostlib_name(name))
}

pub fn is_builtin_with_privileged_wire(name: &str, allow_privileged_wire: bool) -> bool {
    lookup_with_privileged_wire(name, allow_privileged_wire).is_some()
        || (crate::legacy_ambient_capabilities_enabled()
            && crate::is_registered_legacy_hostlib_name(name))
}

/// Every builtin name. Installed names come first, then any static-only
/// names that aren't shadowed by installed entries. Output is NOT
/// alphabetically sorted (callers that need that re-sort themselves).
pub fn iter_builtin_names() -> impl Iterator<Item = &'static str> {
    let installed: Vec<_> = harn_builtin_registry::installed_manifest()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.contract.exposure,
                BuiltinExposure::PureGlobal | BuiltinExposure::CapabilityFunction { .. }
            )
        })
        .collect();
    let installed_names: std::collections::HashSet<&'static str> =
        harn_builtin_registry::installed_manifest()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
    installed.into_iter().map(|entry| entry.name).chain(
        signatures::groups()
            .into_iter()
            .flat_map(|g| g.iter())
            .filter(move |s| !installed_names.contains(s.name))
            .map(|s| s.name),
    )
}

/// Names that come *only* from the hand-written static fallback tables
/// (`signatures::groups()`), independent of whatever the driver installed.
///
/// Exposed so cross-crate drift guards (see the builtin-registry alignment
/// test in `harn-vm`) can assert the static tables never overlap with
/// `#[harn_builtin]`-published or `runtime_only` macro builtins — the exact
/// duplication that let LLM config signatures silently drift before the
/// shapes-in-`harn-builtin-meta` migration.
pub fn static_signature_names() -> impl Iterator<Item = &'static str> {
    signatures::groups()
        .into_iter()
        .flat_map(|g| g.iter())
        .map(|s| s.name)
}

/// Iterate over every builtin's name and statically-known return-type
/// strings. Used by `harn-lint` and other consumers that want a
/// lightweight "what does this builtin return" view without bringing in
/// the full type IR.
pub fn iter_builtin_metadata() -> impl Iterator<Item = BuiltinMetadata> {
    let installed: Vec<_> = harn_builtin_registry::installed_manifest()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.contract.exposure,
                BuiltinExposure::PureGlobal | BuiltinExposure::CapabilityFunction { .. }
            )
        })
        .collect();
    let installed_names: std::collections::HashSet<&'static str> =
        harn_builtin_registry::installed_manifest()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
    installed
        .into_iter()
        .map(|entry| BuiltinMetadata {
            name: entry.name,
            return_types: builtin_return_type_names(entry.signature),
        })
        .chain(
            signatures::groups()
                .into_iter()
                .flat_map(|g| g.iter())
                .filter(move |s| !installed_names.contains(s.name))
                .map(|sig| BuiltinMetadata {
                    name: sig.name,
                    return_types: builtin_return_type_names(sig),
                }),
        )
}

/// Statically-known return type for `name`, materialized as a [`TypeExpr`].
/// Returns `None` for unknown names AND for builtins whose return type is
/// genuinely dynamic ([`Ty::Any`]).
pub fn builtin_return_type(name: &str) -> Option<TypeExpr> {
    let sig = lookup(name)?;
    if sig.returns.is_any() {
        return None;
    }
    Some(sig.returns.to_type_expr())
}

/// Returns true if this builtin produces an untyped/opaque value that
/// should be validated before field access in strict types mode.
///
/// This is the same set the linter's `untyped-dict-access` rule treats
/// as boundary sources — JSON parsing, HTTP responses, LLM outputs,
/// host capability calls, etc.
pub fn is_untyped_boundary_source(name: &str) -> bool {
    matches!(
        name,
        "json_parse"
            | "json_extract"
            | "yaml_parse"
            | "toml_parse"
            | "llm_call"
            | "llm_call_safe"
            | "llm_completion"
            | "http_get"
            | "http_post"
            | "http_put"
            | "http_patch"
            | "http_delete"
            | "http_download"
            | "http_request"
            | "http_session_request"
            | "http_stream_info"
            | "sse_receive"
            | "sse_server_mock_receive"
            | "sse_server_response"
            | "sse_server_status"
            | "websocket_accept"
            | "websocket_receive"
            | "host_call"
            | "connector_call"
            | "host_tool_call"
    )
}

/// Convert the signature's return type to a tiny `&'static [&'static str]`
/// view used by `BuiltinMetadata` consumers (linter, LSP) that don't
/// pull in the full type IR. Only basic primitive names and the common
/// `T | nil` unions are exposed; everything else returns an empty slice
/// so callers know to consult [`builtin_return_type`] instead.
fn builtin_return_type_names(sig: &BuiltinSignature) -> &'static [&'static str] {
    match &sig.returns {
        Ty::Named(name) => match *name {
            "bool" => &["bool"],
            "bytes" => &["bytes"],
            "dict" => &["dict"],
            "float" => &["float"],
            "int" => &["int"],
            "list" => &["list"],
            "nil" => &["nil"],
            "string" => &["string"],
            _ => &[],
        },
        Ty::Union(members) => match *members {
            [Ty::Named("string"), Ty::Named("nil")] => &["string", "nil"],
            [Ty::Named("nil"), Ty::Named("string")] => &["string", "nil"],
            [Ty::Named("int"), Ty::Named("nil")] => &["int", "nil"],
            [Ty::Named("nil"), Ty::Named("int")] => &["int", "nil"],
            [Ty::Named("dict"), Ty::Named("nil")] => &["dict", "nil"],
            [Ty::Named("nil"), Ty::Named("dict")] => &["dict", "nil"],
            [Ty::Named("bytes"), Ty::Named("nil")] => &["bytes", "nil"],
            [Ty::Named("nil"), Ty::Named("bytes")] => &["bytes", "nil"],
            _ => &[],
        },
        Ty::Never => &["never"],
        _ => &[],
    }
}

#[cfg(test)]
mod ambient_install_regression {
    use super::*;

    #[test]
    fn installed_manifest_does_not_shadow_ambient_capability_methods() {
        std::env::set_var("HARN_LEGACY_AMBIENT_CAPABILITIES", "1");
        assert!(
            is_builtin("store_set"),
            "capability-contracts fallback must resolve ambient store_set"
        );

        // Project the same contracts the CLI installs before `harn check`.
        let entries: &'static [&'static harn_builtin_registry::BuiltinManifestEntry] = Box::leak(
            harn_capability_contracts::manifest()
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        harn_builtin_registry::install_builtin_manifest(entries);

        assert!(
            is_builtin("store_set"),
            "after manifest install, ambient store_set must still resolve uniquely"
        );
        assert!(
            legacy_capability_method_entry("store_set").is_some(),
            "legacy_capability_method_entry must stay unique after install"
        );
    }
}
