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
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    const HELPER_MODE_ENV: &str = "HARN_PACKAGE_SNAPSHOT_LEASE_HELPER_MODE";
    const HELPER_ROOT_ENV: &str = "HARN_PACKAGE_SNAPSHOT_LEASE_HELPER_ROOT";
    const HELPER_TEST: &str =
        "stdlib::package_snapshot::tests::package_snapshot_lease_lifecycle_helper";

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

    fn spawn_helper(
        root: &std::path::Path,
        mode: &str,
    ) -> (Child, ChildStdin, BufReader<ChildStdout>) {
        // On macOS, flock leases survive fork until the child execs and applies
        // CLOEXEC. A concurrent Command::spawn elsewhere in the suite can
        // therefore inherit a lease owned by this test process just before the
        // VM drops it. Keep ownership in a helper process so unrelated suite
        // children cannot inherit the descriptor; pipe checkpoints keep the
        // helper alive while the parent verifies each lifecycle boundary.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(HELPER_TEST)
            .arg("--nocapture")
            .env(HELPER_MODE_ENV, mode)
            .env(HELPER_ROOT_ENV, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        (child, stdin, stdout)
    }

    fn wait_for_helper(stdout: &mut impl BufRead, expected: &str) {
        let mut line = String::new();
        loop {
            let read = stdout.read_line(&mut line).unwrap();
            assert_ne!(
                read, 0,
                "package snapshot lease helper exited before {expected:?}"
            );
            if line.trim() == expected {
                return;
            }
            line.clear();
        }
    }

    fn continue_helper(stdin: &mut impl Write) {
        stdin.write_all(b"continue\n").unwrap();
        stdin.flush().unwrap();
    }

    fn helper_checkpoint(marker: &str) {
        println!("{marker}");
        std::io::stdout().flush().unwrap();

        let mut command = String::new();
        std::io::stdin().read_line(&mut command).unwrap();
        assert_eq!(command.trim(), "continue");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn package_snapshot_lease_lifecycle_helper() {
        let Ok(mode) = std::env::var(HELPER_MODE_ENV) else {
            return;
        };
        let root = PathBuf::from(std::env::var_os(HELPER_ROOT_ENV).unwrap());
        let mut owner = Vm::new();
        crate::register_vm_stdlib(&mut owner);

        match mode.as_str() {
            "drop" => {
                let chunk = crate::compile_source(&open_source(&root)).unwrap();
                let receipt = owner.execute(&chunk).await.unwrap();
                assert!(matches!(receipt, VmValue::Dict(_)));
                helper_checkpoint("snapshot-open");

                std::thread::spawn(crate::reset_thread_local_state)
                    .join()
                    .unwrap();
                helper_checkpoint("unrelated-reset");

                drop(owner);
                helper_checkpoint("owner-dropped");
            }
            "close" => {
                let root = serde_json::to_string(&root.to_string_lossy()).unwrap();
                let chunk = crate::compile_source(&format!(
                    "fn main(harness: Harness) {{\n  const snapshot = package_snapshot_open({root})\n  return package_snapshot_close(snapshot.handle)\n}}\nmain(harness)"
                ))
                .unwrap();
                assert!(matches!(
                    owner.execute(&chunk).await.unwrap(),
                    VmValue::Bool(true)
                ));
                helper_checkpoint("snapshot-closed");
            }
            other => panic!("unknown package snapshot lease helper mode {other:?}"),
        }
    }

    #[test]
    fn another_execution_reset_cannot_release_live_snapshot_lease() {
        let temp = tempfile::tempdir().unwrap();
        let lease_path = publish_fixture(temp.path());
        let (mut helper, mut helper_stdin, mut helper_stdout) = spawn_helper(temp.path(), "drop");
        let lease = File::open(&lease_path).unwrap();
        wait_for_helper(&mut helper_stdout, "snapshot-open");
        assert!(
            lease.try_lock().is_err(),
            "the open snapshot must hold its generation lease"
        );

        continue_helper(&mut helper_stdin);
        wait_for_helper(&mut helper_stdout, "unrelated-reset");
        assert!(
            lease.try_lock().is_err(),
            "another execution reset must not release the owner's live lease"
        );

        continue_helper(&mut helper_stdin);
        wait_for_helper(&mut helper_stdout, "owner-dropped");
        lease
            .try_lock()
            .expect("dropping the owning execution must release its abandoned reader lease");
        continue_helper(&mut helper_stdin);
        assert!(helper.wait().unwrap().success());
    }

    #[test]
    fn explicit_close_releases_owning_execution_lease() {
        let temp = tempfile::tempdir().unwrap();
        let lease_path = publish_fixture(temp.path());
        let (mut helper, mut helper_stdin, mut helper_stdout) = spawn_helper(temp.path(), "close");
        wait_for_helper(&mut helper_stdout, "snapshot-closed");
        let lease = File::open(lease_path).unwrap();
        lease
            .try_lock()
            .expect("explicit close must release the owning execution's reader lease");
        continue_helper(&mut helper_stdin);
        assert!(helper.wait().unwrap().success());
    }
}
