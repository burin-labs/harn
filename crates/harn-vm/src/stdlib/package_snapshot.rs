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

/// Release every package-generation reader lease left open by the completed
/// Harn run. Normal scripts close handles with `defer`; this is the error and
/// cancellation backstop for runs that never reach their defer stack.
pub(crate) fn reset_package_snapshot_state() {
    OPEN_PACKAGE_SNAPSHOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    use fs2::FileExt;
    use harn_modules::package_snapshot::{
        package_current_path, package_generations_dir, package_lock_digest,
        package_publication_lock_path, package_state_dir, PackageGenerationManifest,
        PackageGenerationPointer, GENERATION_LEASE_FILE, GENERATION_LOCK_FILE,
        GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };

    fn publish_fixture(root: &std::path::Path) -> PathBuf {
        const GENERATION: &str = "reset_lease_generation";
        let generation_root = package_generations_dir(root).join(GENERATION);
        fs::create_dir_all(generation_root.join(GENERATION_PACKAGES_DIR)).unwrap();
        let lock_body = b"version = 4\n";
        fs::write(generation_root.join(GENERATION_LOCK_FILE), lock_body).unwrap();
        fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
        let manifest =
            PackageGenerationManifest::new(GENERATION, package_lock_digest(lock_body)).unwrap();
        fs::write(
            generation_root.join(GENERATION_MANIFEST_FILE),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(package_state_dir(root)).unwrap();
        fs::write(
            package_current_path(root),
            toml::to_string_pretty(&PackageGenerationPointer::new(GENERATION).unwrap()).unwrap(),
        )
        .unwrap();
        File::create(package_publication_lock_path(root)).unwrap();
        generation_root.join(GENERATION_LEASE_FILE)
    }

    #[test]
    fn package_snapshot_process_global_lifetime_ends_at_thread_reset() {
        reset_package_snapshot_state();
        let temp = tempfile::tempdir().unwrap();
        let lease_path = publish_fixture(temp.path());
        let snapshot = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        OPEN_PACKAGE_SNAPSHOTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("abandoned-run".to_string(), snapshot);

        let lease = File::open(lease_path).unwrap();
        assert!(
            FileExt::try_lock_exclusive(&lease).is_err(),
            "the open snapshot must hold its generation lease"
        );

        crate::reset_thread_local_state();

        FileExt::try_lock_exclusive(&lease).expect("reset must release the abandoned reader lease");
    }
}
