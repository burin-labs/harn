use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_modules::package_snapshot::PackageSnapshot;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

#[derive(Default)]
pub(crate) struct PackageSnapshotRegistry {
    snapshots: Mutex<BTreeMap<String, PackageSnapshot>>,
}

impl PackageSnapshotRegistry {
    fn insert(&self, handle: String, snapshot: PackageSnapshot) -> Result<(), VmError> {
        self.snapshots
            .lock()
            .map_err(|_| VmError::Runtime("package snapshot store is poisoned".to_string()))?
            .insert(handle, snapshot);
        Ok(())
    }

    fn remove(&self, handle: &str) -> Result<bool, VmError> {
        Ok(self
            .snapshots
            .lock()
            .map_err(|_| VmError::Runtime("package snapshot store is poisoned".to_string()))?
            .remove(handle)
            .is_some())
    }
}

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
    kind = "async",
    category = "fs",
    doc = "Open the current immutable package generation and hold its reader lease until package_snapshot_close."
)]
async fn package_snapshot_open_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    ctx.package_snapshot_registry().insert(handle, snapshot)?;
    Ok(VmValue::dict(receipt))
}

#[harn_builtin(
    sig = "package_snapshot_close(handle: string) -> bool",
    kind = "async",
    category = "fs",
    doc = "Release a package generation reader lease opened by package_snapshot_open."
)]
async fn package_snapshot_close_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let handle = args.first().map(VmValue::display).unwrap_or_default();
    Ok(VmValue::Bool(
        ctx.package_snapshot_registry().remove(&handle)?,
    ))
}

fn display_path(path: &std::path::Path) -> String {
    PathBuf::from(path).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_modules::package_snapshot::{
        package_current_path, package_generations_dir, package_lock_digest,
        package_publication_lock_path, package_state_dir, PackageGenerationManifest,
        PackageGenerationPointer, GENERATION_LEASE_FILE, GENERATION_LOCK_FILE,
        GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };
    use std::fs::{self, File};

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

    fn open_source(root: &std::path::Path) -> String {
        format!(
            "fn main(harness: Harness) {{ return package_snapshot_open({}) }}\nmain(harness)",
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn another_execution_reset_cannot_release_live_snapshot_lease() {
        let temp = tempfile::tempdir().unwrap();
        let lease_path = publish_fixture(temp.path());
        let chunk = crate::compile_source(&open_source(temp.path())).unwrap();
        let mut owner = Vm::new();
        crate::register_vm_stdlib(&mut owner);
        let receipt = owner.execute(&chunk).await.unwrap();
        let VmValue::Dict(_) = receipt else {
            panic!("package snapshot open must return a receipt");
        };

        let lease = File::open(&lease_path).unwrap();
        assert!(
            lease.try_lock().is_err(),
            "the open snapshot must hold its generation lease"
        );

        std::thread::spawn(crate::reset_thread_local_state)
            .join()
            .unwrap();
        assert!(
            lease.try_lock().is_err(),
            "another execution reset must not release the owner's live lease"
        );

        drop(owner);
        lease
            .try_lock()
            .expect("dropping the owning execution must release its abandoned reader lease");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_close_releases_owning_execution_lease() {
        let temp = tempfile::tempdir().unwrap();
        let lease_path = publish_fixture(temp.path());
        let root = serde_json::to_string(&temp.path().to_string_lossy()).unwrap();
        let chunk = crate::compile_source(&format!(
            "fn main(harness: Harness) {{\n  const snapshot = package_snapshot_open({root})\n  return package_snapshot_close(snapshot.handle)\n}}\nmain(harness)"
        ))
        .unwrap();
        let mut owner = Vm::new();
        crate::register_vm_stdlib(&mut owner);

        assert!(matches!(
            owner.execute(&chunk).await.unwrap(),
            VmValue::Bool(true)
        ));
        let lease = File::open(lease_path).unwrap();
        lease
            .try_lock()
            .expect("explicit close must release the owning execution's reader lease");
    }
}
