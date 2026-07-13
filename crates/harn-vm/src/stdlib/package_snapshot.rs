use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use harn_modules::package_snapshot::PackageSnapshot;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

static OPEN_PACKAGE_SNAPSHOTS: LazyLock<Mutex<BTreeMap<String, PackageSnapshot>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &PACKAGE_SNAPSHOT_OPEN_BUILTIN_DEF,
    &PACKAGE_SNAPSHOT_CLOSE_BUILTIN_DEF,
];

pub(crate) fn register_package_snapshot_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "package_snapshot_open(project_root: string) -> {handle: string, generation: string, packages_root: string, lock_path: string, lock_digest: string, packages: list<string>}?",
    category = "fs",
    doc = "Open the current immutable package generation and hold its reader lease until package_snapshot_close."
)]
fn package_snapshot_open_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let project_root = args
        .first()
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("package_snapshot_open requires project_root".to_string())
        })?;
    let project_root = crate::stdlib::process::resolve_source_relative_path(&project_root);
    crate::stdlib::sandbox::enforce_fs_path(
        "package_snapshot_open",
        &project_root,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;

    let Some(snapshot) = PackageSnapshot::acquire(&project_root).map_err(|error| {
        VmError::Runtime(format!(
            "package_snapshot_open {}: {error}",
            project_root.display()
        ))
    })?
    else {
        return Ok(VmValue::Nil);
    };

    let handle = format!("package_snapshot_{}", uuid::Uuid::now_v7().simple());
    let mut receipt = crate::value::DictMap::new();
    receipt.put_str("handle", &handle);
    receipt.put_str("generation", snapshot.generation());
    receipt.put_str("packages_root", display_path(snapshot.packages_root()));
    receipt.put_str("lock_path", display_path(snapshot.lock_path()));
    receipt.put_str("lock_digest", snapshot.lock_digest());
    receipt.put(
        "packages",
        VmValue::List(Arc::new(
            snapshot
                .package_names()
                .iter()
                .map(VmValue::string)
                .collect(),
        )),
    );
    OPEN_PACKAGE_SNAPSHOTS
        .lock()
        .map_err(|_| VmError::Runtime("package snapshot store is poisoned".to_string()))?
        .insert(handle, snapshot);
    Ok(VmValue::dict(receipt))
}

#[harn_builtin(
    sig = "package_snapshot_close(handle: string) -> bool",
    category = "fs",
    doc = "Release a package generation reader lease opened by package_snapshot_open."
)]
fn package_snapshot_close_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let handle = args.first().map(VmValue::display).unwrap_or_default();
    let removed = OPEN_PACKAGE_SNAPSHOTS
        .lock()
        .map_err(|_| VmError::Runtime("package snapshot store is poisoned".to_string()))?
        .remove(&handle);
    Ok(VmValue::Bool(removed.is_some()))
}

fn display_path(path: &std::path::Path) -> String {
    PathBuf::from(path).to_string_lossy().into_owned()
}
