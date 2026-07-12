use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::stdlib::process::resolve_source_relative_path;
use crate::stdlib::project_catalog::{project_catalog, ProjectCatalogEntry};
use crate::stdlib::project_enrich::register_project_enrich_builtin;
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

use super::*;

pub(crate) fn register_project_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    register_project_enrich_builtin(vm);
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &PROJECT_CONTEXT_PROFILE_NATIVE_IMPL_DEF,
    &PROJECT_FINGERPRINT_IMPL_DEF,
    &PROJECT_SCAN_NATIVE_IMPL_DEF,
    &PROJECT_SCAN_TREE_NATIVE_IMPL_DEF,
    &PROJECT_WALK_TREE_NATIVE_IMPL_DEF,
    &PROJECT_CATALOG_NATIVE_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "project_context_profile_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_context_profile_native_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if args.len() > 2 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "project_context_profile: expected at most 2 arguments",
        ))));
    }
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_context_profile_options(args.get(1));
    let root = if options.fingerprint.is_none() {
        resolve_existing_directory(&path)?
    } else {
        resolve_source_relative_path(&path)
            .canonicalize()
            .unwrap_or_else(|_| resolve_source_relative_path(&path))
    };
    Ok(resolve_context_profile(&root, options).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_fingerprint(path?: string) -> ProjectFingerprint",
    category = "project"
)]
fn project_fingerprint_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() > 1 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "project_fingerprint: expected at most 1 argument",
        ))));
    }
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let root = resolve_existing_directory(&path)?;
    Ok(detect_project_fingerprint(&root).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_scan_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_scan_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let root = resolve_existing_directory(&path)?;
    Ok(scan_exact_directory(&root, &options).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_scan_tree_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_scan_tree_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let base = resolve_existing_directory(&path)?;
    let tree = scan_project_tree(&base, &options)?;
    Ok(VmValue::dict(
        tree.into_iter()
            .map(|(rel, evidence)| (rel, evidence.into_vm_value()))
            .collect::<crate::value::DictMap>(),
    ))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_walk_tree_native(path?: string, options?: dict) -> list",
    category = "project"
)]
fn project_walk_tree_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let base = resolve_existing_directory(&path)?;
    let tree = walk_project_tree(&base, &options)?;
    Ok(VmValue::List(std::sync::Arc::new(
        tree.into_iter()
            .map(ProjectTreeEntry::into_vm_value)
            .collect(),
    )))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_catalog_native() -> list",
    category = "project"
)]
fn project_catalog_native_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let entries = project_catalog()
        .iter()
        .map(catalog_entry_value)
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

pub(crate) fn project_scan_config_value(dir: &Path) -> VmValue {
    let mut options = ProjectScanOptions::default();
    options.tiers.insert(ScanTier::Config);
    scan_exact_directory(dir, &options).into_vm_value()
}

fn resolve_existing_directory(path: &str) -> Result<PathBuf, VmError> {
    let resolved = resolve_source_relative_path(path);
    let target = if resolved.is_dir() {
        resolved
    } else {
        resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    if target.exists() {
        target.canonicalize().map_err(path_error)
    } else {
        Err(path_missing_error(&target))
    }
}

fn path_error(error: std::io::Error) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "project.scan: failed to resolve path: {error}"
    ))))
}

fn path_missing_error(path: &Path) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "project.scan: path does not exist: {}",
        path.display()
    ))))
}

fn catalog_entry_value(entry: &ProjectCatalogEntry) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_str("id", entry.id);
    value.insert(
        "languages".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .languages
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "frameworks".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .frameworks
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "build_systems".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .build_systems
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "anchors".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .anchors
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "lockfiles".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .lockfiles
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "source_globs".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .source_globs
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "default_build_cmd".to_string(),
        entry
            .default_build_cmd
            .map(|value| VmValue::String(arcstr::ArcStr::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "default_test_cmd".to_string(),
        entry
            .default_test_cmd
            .map(|value| VmValue::String(arcstr::ArcStr::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(value)
}
